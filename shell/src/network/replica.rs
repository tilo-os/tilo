//! Glue between the in-process [`Hub`] (Phase 1 Task 1.2) and the
//! [`WorkspaceTree`]'s automerge sync API (Phase 1 Task 1.3 / unit A of
//! #157). A [`WorkspaceTreeReplica`] wraps a tree + a peer id + a list of
//! "outbound" deliveries (tests can drain) and implements
//! [`TopicSubscriber`] so a [`Hub`] can fan messages to it.
//!
//! # Replication is per room
//!
//! A peer invited to one workspace is in one room. It is not entitled to
//! the others on this machine — not their names, not their room ids, not
//! their owners, and least of all their capability sets, which say which
//! app holds which grant over paths the guest cannot reach.
//!
//! So the unit of replication is a room, and the document a peer receives
//! is that room's own ([`WorkspaceTree::export_room_changes_since`]). The
//! tree-wide document is never exported here. That is a stronger statement
//! than "the export is filtered": there is no filter to get wrong, because
//! the document handed over never contained anything else.
//!
//! The protocol behaviour:
//!   - `SyncReq { since, room }` naming a room this machine is in → reply
//!     `SyncResp { changes, room }` carrying that room's changes since
//!     `since`. The reply lands in `pending_outgoing` for the test (or
//!     production loopback driver) to forward via [`Hub::broadcast`].
//!   - `SyncReq` naming a room this machine is not in → no reply. Nothing
//!     is exported and nothing is said about whether the room exists.
//!   - `SyncReq` naming no room at all — the kick the transport broadcasts
//!     on peer-join, before it knows what anyone is in — exports nothing
//!     and instead makes this replica name its own rooms, one
//!     `SyncReq` each. That is what turns a blind join into a sync round
//!     without any layer having to hold a list of everybody's rooms.
//!   - `SyncResp { changes, room }` → `tree.apply_room_changes(room, …)`,
//!     which refuses a room this machine is not in before it stores
//!     anything.
//!   - All other variants are ignored at this layer; later phases route
//!     them.
//!
//! [`Hub`]: crate::network::hub::Hub
//! [`Hub::broadcast`]: crate::network::hub::Hub::broadcast
//! [`TopicSubscriber`]: crate::network::hub::TopicSubscriber
//! [`WorkspaceTree`]: crate::shell::state::WorkspaceTree

use std::sync::{Arc, Mutex};

use crate::network::hub::TopicSubscriber;
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::{PeerOrigin, RoomId};
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
use crate::shell::state::WorkspaceTreeHandle;

/// Replica-side glue: owns a (shared) [`WorkspaceTree`] **handle**, a
/// peer id, and a FIFO queue of messages to be broadcast on its behalf.
///
/// C-3: the tree is held behind a [`WorkspaceTreeHandle`]
/// (`Arc<parking_lot::RwLock<WorkspaceTree>>`) shared with the shell
/// chrome and the portal's grant store, so an `apply_changes` call lands
/// on the same tree the chrome reads from on its next paint and the
/// Permissions tab observes immediately.
///
/// `pending_outgoing` stays under `std::sync::Mutex` — the actor / hub
/// drivers drain it from arbitrary threads and there's no read-heavy
/// workload to motivate `RwLock`.
///
/// [`Hub`]: crate::network::hub::Hub
pub struct WorkspaceTreeReplica {
    pub id: u64,
    pub tree: WorkspaceTreeHandle,
    pub pending_outgoing: Arc<Mutex<Vec<NeighbourMsg>>>,
    /// The transport identity this replica's messages arrive under.
    ///
    /// Only meaningful when the replica is driven through a [`Hub`],
    /// which has no peer identities of its own: the live wiring calls
    /// [`Self::handle_from`] with the authenticated `PeerId` directly and
    /// never consults this. A hub-driven replica standing in for a peer —
    /// the one that issued an invite, say — sets it so the receiving side
    /// sees the identity the invite named.
    ///
    /// [`Hub`]: crate::network::hub::Hub
    pub origin: PeerOrigin,
}

impl WorkspaceTreeReplica {
    pub fn new(id: u64, tree: WorkspaceTreeHandle) -> Self {
        Self {
            id,
            tree,
            pending_outgoing: Arc::new(Mutex::new(Vec::new())),
            origin: PeerOrigin::hub(id),
        }
    }

    /// Deliver this replica's messages under `origin`.
    pub fn with_origin(mut self, origin: PeerOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Stash one `SyncReq` per room this machine is in, each carrying that
    /// room's current heads.
    ///
    /// A machine asks room by room because it is answered room by room: a
    /// request naming several rooms would be answered with one document
    /// merged out of several, and the receiver could no longer tell which
    /// room anything in it belonged to. A machine in no room asks for
    /// nothing, which is correct — it has nothing to be caught up on.
    ///
    /// Both the reconnect path and the tests kick a round with this.
    pub fn request_sync(&self) {
        let mut tree = self.tree.write();
        let rooms = tree.rooms();
        let mut msgs = Vec::with_capacity(rooms.len());
        for room in rooms {
            let heads = tree.room_heads(room);
            // D-8: `serde_json::to_vec(&Vec<ChangeHash>)` is structurally
            // infallible — there's no enum variant or non-finite float in
            // `ChangeHash`. We keep `unwrap_or_default()` for defence in
            // depth and log loudly if it ever trips; the fallback
            // `vec![]` is interpreted by the recipient as "send me every
            // change **in this room**", which is wasteful but not
            // corrupting, and cannot widen the request past the room it
            // names. This is the intentional silent-error path: the
            // failure mode is bandwidth, not data loss.
            let since = serde_json::to_vec(&heads).unwrap_or_else(|err| {
                tracing::error!(
                    ?err,
                    "serde_json::to_vec(&heads) failed; falling back to a \
                     full-sync request for this room (D-8). This should be \
                     unreachable; if you see this in production, file an issue."
                );
                Vec::new()
            });
            msgs.push(NeighbourMsg::SyncReq {
                since,
                room: room.to_string(),
            });
        }
        drop(tree);
        self.pending_outgoing.lock().unwrap().extend(msgs);
    }

    /// Drain pending outbound messages so a test (or a Hub-driver) can
    /// broadcast them. Stable order: FIFO insertion.
    pub fn drain_outgoing(&self) -> Vec<NeighbourMsg> {
        std::mem::take(&mut *self.pending_outgoing.lock().unwrap())
    }
}

impl TopicSubscriber for WorkspaceTreeReplica {
    fn id(&self) -> u64 {
        self.id
    }

    fn origin(&self) -> PeerOrigin {
        self.origin.clone()
    }

    fn handle(&mut self, sender: &PeerOrigin, msg: &NeighbourMsg) {
        self.handle_from(sender, msg);
    }
}

impl WorkspaceTreeReplica {
    /// [`TopicSubscriber::handle`] under the name the live transport
    /// authenticated, which is what an authority decision may rest on.
    ///
    /// The room-replication path decides whether a payload may rewrite a
    /// room's governance, and that decision turns on **who** the bytes
    /// came from. The `u64` the hub used to pass cannot carry that: in
    /// production it was a `DefaultHasher` digest of the peer id, fine for
    /// echo-suppression and not fine for an equality check an attacker
    /// gets to aim at. So the live wiring calls this with the peer's own
    /// identity ([`PeerOrigin::peer`]).
    pub fn handle_from(&mut self, from: &PeerOrigin, msg: &NeighbourMsg) {
        let sender = from;
        match msg {
            NeighbourMsg::SyncReq { since, room } => {
                // A request that names no room is the transport's
                // peer-join kick. It is not a request for everything —
                // answering it with a document would be the leak this
                // whole path exists to close — so it exports nothing and
                // is answered by naming our own rooms instead.
                let Some(room_id) = RoomId::parse(room).filter(|r| !r.is_nil()) else {
                    self.request_sync();
                    return;
                };
                let since_heads: Vec<automerge::ChangeHash> =
                    serde_json::from_slice(since).unwrap_or_default();
                let mut tree = self.tree.write();
                let exported = tree.export_room_changes_since(room_id, &since_heads);
                drop(tree);
                // `None` is "we are not in that room". No reply: a peer
                // learns nothing about a room it named and we do not have,
                // not even that it is unknown here.
                let Some(changes) = exported else {
                    return;
                };
                if changes.is_empty() {
                    return;
                }
                self.pending_outgoing
                    .lock()
                    .unwrap()
                    .push(NeighbourMsg::SyncResp {
                        changes,
                        room: room_id.to_string(),
                    });
            }
            NeighbourMsg::SyncResp { changes, room } => {
                // C-9 fix: surface `apply_changes` failures via
                // `tracing::error!` so peer-sync regressions show up in
                // production logs. The previous code dropped the
                // `Result` on the floor, leaving the local replica to
                // silently believe a sync round had succeeded while
                // the doc remained un-updated. We keep the function's
                // semantics ("don't propagate; peer connection stays
                // alive") — the log line is the only visible side
                // effect of the failure path.
                //
                // C-3: `WorkspaceTree::apply_room_changes` folds the
                // merged room back into the cached workspace, so the
                // cached fields stay in sync with what arrived. That's
                // the critical correctness invariant the RFC names —
                // every subsystem reading through the shared handle
                // observes the merged state.
                //
                // A response naming a room we are not in is refused
                // there, before anything is stored: an uninvited
                // document is never held, not held and then ignored.
                let Some(room_id) = RoomId::parse(room).filter(|r| !r.is_nil()) else {
                    tracing::warn!(
                        target: "shell::network::replica",
                        sender = %sender,
                        "SyncResp names no room; dropped",
                    );
                    return;
                };
                if let Err(err) = self.tree.write().apply_room_changes(room_id, changes, from) {
                    tracing::error!(
                        target: "shell::network::replica",
                        sender = %sender,
                        room = %room_id,
                        change_count = changes.len(),
                        ?err,
                        "WorkspaceTreeReplica::apply_room_changes failed; \
                         peer-sync round produced no doc mutation (C-9)",
                    );
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    //! Two-replica convergence over a [`Hub`]. The hub owns its
    //! subscribers (via `Box<dyn TopicSubscriber>`), so to keep a handle
    //! after attach we wrap each replica in `Arc<Mutex<_>>` and attach a
    //! thin proxy that forwards `TopicSubscriber` calls — the same
    //! pattern used by the hub's own tests.
    //!
    //! [`Hub`]: crate::network::hub::Hub
    use super::*;
    use crate::network::hub::Hub;
    use automerge::{ROOT, ReadDoc, ScalarValue};

    /// Proxy that forwards `TopicSubscriber` calls into a shared replica
    /// behind `Arc<Mutex<_>>`. Mirrors the `ArcSub` helper in
    /// `hub.rs`'s tests but specialised for `WorkspaceTreeReplica`.
    struct ArcReplica(Arc<Mutex<WorkspaceTreeReplica>>);

    impl TopicSubscriber for ArcReplica {
        fn id(&self) -> u64 {
            self.0.lock().unwrap().id()
        }
        fn origin(&self) -> PeerOrigin {
            self.0.lock().unwrap().origin()
        }
        fn handle(&mut self, sender: &PeerOrigin, msg: &NeighbourMsg) {
            self.0.lock().unwrap().handle(sender, msg);
        }
    }


    // =================================================================
    // The claim under test
    //
    // A peer invited to room A holds nothing belonging to room B.
    //
    // "Holds" is read off the guest's own state after a real sync round,
    // not off the host's send path: `holdings` walks the guest's cache,
    // its tree document (materialised **and** its raw change bytes) and
    // every room document it has, and the room-B assertions are that
    // room B's name, room id, owner and capability appear in none of it.
    // =================================================================

    use crate::network::invite::{accept_invite_into, make_invite_token};
    use crate::shell::state::WorkspaceId;
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};

    /// Room A's distinguishing strings. Chosen so a substring search
    /// cannot match them by accident.
    const ROOM_A_NAME: &str = "Design review";
    const ROOM_A_OWNER: &str = "alice-owns-room-a";
    const ROOM_A_PATH: &str = "/home/user/room-a-notes";
    /// Room B's. Nothing in this list may ever reach the guest.
    const ROOM_B_NAME: &str = "Payroll";
    const ROOM_B_OWNER: &str = "bob-owns-room-b";
    const ROOM_B_PATH: &str = "/srv/room-b-secrets";

    /// A host with two rooms: the one it will invite a guest into, and one
    /// it will not. Both are fully furnished — named, owned, and carrying
    /// a capability — so "nothing of room B" is a claim with something to
    /// be false about.
    ///
    /// Returns the tree plus the two local workspace handles.
    fn host_with_two_rooms() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let mut tree = WorkspaceTree::default();
        let a = tree.append_empty_workspace(ROOM_A_NAME);
        let b = tree.append_empty_workspace(ROOM_B_NAME);

        // Ownership through the production path, then re-point each room
        // at its own owner so the two are distinguishable on the wire.
        tree.adopt_ownerless_workspaces(&ROOM_A_OWNER.to_string());
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == b) {
            ws.owner = Some(ROOM_B_OWNER.to_string());
        }

        tree.grant_in_workspace(
            a,
            &ROOM_A_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(ROOM_A_PATH.to_string()),
                Verb::Read,
            ),
        )
        .expect("the owner may grant in room A");
        tree.grant_in_workspace(
            b,
            &ROOM_B_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(ROOM_B_PATH.to_string()),
                Verb::Read,
            ),
        )
        .expect("the owner may grant in room B");
        tree.seed_doc_from_cache();
        (tree, a, b)
    }

    /// Everything a tree holds, flattened into one string to search.
    ///
    /// Three sources, because a leak could land in any of them: the cache
    /// (which is what the shell reads and what persists), the tree
    /// document (materialised at its current heads *and* the raw bytes of
    /// every change in its graph, so a value that arrived and was later
    /// overwritten is still caught), and every room document.
    fn holdings(tree: &mut WorkspaceTree) -> String {
        let mut out = serde_json::to_string(&*tree).expect("the cache serialises");
        dump_doc(&mut tree.doc, &mut out);
        let rooms: Vec<crate::shell::state::RoomId> = tree.room_docs.keys().copied().collect();
        for room in rooms {
            out.push_str(&room.to_string());
            let doc = tree.room_docs.get_mut(&room).expect("just enumerated");
            dump_doc(doc, &mut out);
        }
        out
    }

    fn dump_doc(doc: &mut automerge::AutoCommit, out: &mut String) {
        for change in doc.get_changes(&[]) {
            out.push_str(&String::from_utf8_lossy(change.raw_bytes()));
        }
        dump_map(doc, &ROOT, out);
    }

    fn dump_map(doc: &automerge::AutoCommit, obj: &automerge::ObjId, out: &mut String) {
        let keys: Vec<String> = doc.keys(obj).collect();
        for key in keys {
            out.push('\n');
            out.push_str(&key);
            out.push('=');
            if let Some((value, id)) = doc.get(obj, key.as_str()).ok().flatten() {
                dump_value(doc, &value, &id, out);
            }
        }
    }

    fn dump_value(
        doc: &automerge::AutoCommit,
        value: &automerge::Value<'_>,
        id: &automerge::ObjId,
        out: &mut String,
    ) {
        use automerge::{ObjType, Value};
        match value {
            Value::Scalar(s) => match s.as_ref() {
                ScalarValue::Str(t) => out.push_str(t),
                ScalarValue::Bytes(b) => out.push_str(&String::from_utf8_lossy(b)),
                other => out.push_str(&format!("{other:?}")),
            },
            Value::Object(ObjType::List) | Value::Object(ObjType::Text) => {
                for i in 0..doc.length(id) {
                    if let Some((v, child)) = doc.get(id, i).ok().flatten() {
                        dump_value(doc, &v, &child, out);
                    }
                }
            }
            Value::Object(_) => dump_map(doc, id, out),
        }
    }

    /// Run one full sync round between two replicas over a [`Hub`]:
    /// `initiator` asks, the other answers, the answer comes back.
    fn sync_round(
        hub: &mut Hub,
        initiator: &Arc<Mutex<WorkspaceTreeReplica>>,
        others: &[&Arc<Mutex<WorkspaceTreeReplica>>],
    ) {
        // Every queue is drained into a local binding before the hub is
        // pumped: `Hub::broadcast` asks each subscriber for its id, which
        // re-enters the very mutex a still-live `lock()` temporary would
        // be holding.
        initiator.lock().unwrap().request_sync();
        let from = initiator.lock().unwrap().id();
        let outbound = initiator.lock().unwrap().drain_outgoing();
        for msg in outbound {
            hub.broadcast(from, msg);
        }
        for other in others {
            let id = other.lock().unwrap().id();
            let replies = other.lock().unwrap().drain_outgoing();
            for msg in replies {
                hub.broadcast(id, msg);
            }
        }
    }

    /// **The negative.** A guest invited to room A must hold nothing of
    /// room B — not its name, not its room id, not its owner, and not its
    /// capabilities.
    ///
    /// The invite is minted and accepted through the production path
    /// (`make_invite_token` / `accept_invite_into`), the round runs
    /// through `WorkspaceTreeReplica::handle` over a `Hub`, and the
    /// assertions read the guest's own state afterwards.
    #[test]
    fn a_guest_invited_to_one_room_holds_nothing_of_another() {
        let (host_tree, host_a, host_b) = host_with_two_rooms();
        let room_a = host_tree.room_of(host_a).expect("room A");
        let room_b = host_tree.room_of(host_b).expect("room B");

        // The guest: its own machine, its own workspace, its own room —
        // until it accepts the invite.
        let mut guest_tree = WorkspaceTree::default();
        let guest_ws = guest_tree.append_empty_workspace("My workspace");

        let owner_key = libp2p::identity::Keypair::generate_ed25519();
        let token = make_invite_token(
            &owner_key,
            host_tree
                .workspaces
                .iter()
                .find(|w| w.id == host_a)
                .expect("host workspace A"),
            "/ip4/127.0.0.1/tcp/4001",
            3600,
        )
        .expect("the host issues an invite to room A");
        let joined =
            accept_invite_into(&mut guest_tree, guest_ws, &token).expect("the guest accepts it");
        assert_eq!(joined, room_a, "the invite names room A");

        // The host replica delivers under the identity its own invite
        // named. That is what the guest pinned as room A's governance
        // authority when it accepted, and it is why the round below is
        // answered rather than refused: a guest takes a room's owner and
        // capabilities from the peer whose signature admitted it.
        let host = Arc::new(Mutex::new(
            WorkspaceTreeReplica::new(1, host_tree.shared()).with_origin(PeerOrigin::peer(
                &owner_key.public().to_peer_id(),
            )),
        ));
        let guest_handle = guest_tree.shared();
        let guest = Arc::new(Mutex::new(WorkspaceTreeReplica::new(
            2,
            guest_handle.clone(),
        )));

        let mut hub = Hub::new();
        hub.attach(Box::new(ArcReplica(host.clone())));
        hub.attach(Box::new(ArcReplica(guest.clone())));
        sync_round(&mut hub, &guest, &[&host]);

        let held = {
            let mut tree = guest_handle.write();
            holdings(&mut tree)
        };

        // --- The claim. Asserted first, so a regression is reported as
        // the field that leaked rather than as some downstream symptom. ---
        assert_absent(&held, ROOM_B_NAME, "room B's name");
        assert_absent(&held, &room_b.to_string(), "room B's room id");
        assert_absent(&held, ROOM_B_OWNER, "room B's owner");
        assert_absent(&held, ROOM_B_PATH, "room B's capability");
        // And the guest gained no workspace it was not invited to.
        assert_eq!(
            guest_handle.read().workspaces.len(),
            1,
            "the guest has one workspace — its own, now in room A",
        );

        // --- The positive control. Without it, a guest that received
        // nothing at all would satisfy every assertion above. ---
        assert!(
            held.contains(ROOM_A_NAME),
            "the guest must receive room A's name"
        );
        assert!(
            held.contains(ROOM_A_PATH),
            "the guest must receive room A's capability"
        );
        assert!(
            held.contains(ROOM_A_OWNER),
            "the guest must receive room A's owner"
        );
        assert_eq!(
            guest_handle
                .read()
                .workspaces
                .iter()
                .find(|w| w.id == guest_ws)
                .map(|w| w.name.clone()),
            Some(ROOM_A_NAME.to_string()),
            "the round must land in the guest's own workspace, not beside it",
        );
        let _ = room_a;
    }

    /// Fail naming the leaked field, with the surrounding bytes of the
    /// guest's own state as evidence — a whole dump would bury which of
    /// the four assertions tripped.
    fn assert_absent(held: &str, needle: &str, what: &str) {
        let Some(at) = held.find(needle) else {
            return;
        };
        let start = held[..at].char_indices().rev().nth(70).map_or(0, |(i, _)| i);
        let end = held[at..]
            .char_indices()
            .nth(needle.chars().count() + 70)
            .map_or(held.len(), |(i, _)| at + i);
        panic!(
            "{what} ({needle:?}) leaked to a guest invited only to room A.\n\
             Found in the guest's own state at byte {at}:\n…{}…",
            &held[start..end]
        );
    }

    /// The other direction of the same boundary: a request naming a room
    /// this machine is not in is answered with nothing at all, rather than
    /// with whatever the machine happens to have.
    #[test]
    fn a_request_for_a_room_we_are_not_in_is_not_answered() {
        let (host_tree, _a, _b) = host_with_two_rooms();
        let stranger_room = crate::shell::state::RoomId::new();
        let mut host = WorkspaceTreeReplica::new(1, host_tree.shared());

        host.handle(
            &PeerOrigin::hub(9),
            &NeighbourMsg::SyncReq {
                since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new()).unwrap(),
                room: stranger_room.to_string(),
            },
        );
        assert!(
            host.drain_outgoing().is_empty(),
            "a room we are not in must produce no reply — not an empty one, \
             which would still confirm the request was understood",
        );
    }

    /// The transport's peer-join kick names no room, because the
    /// transport does not hold the tree. It must not be read as "send me
    /// everything": it exports nothing, and instead makes this replica
    /// name its own rooms so the round can proceed room by room.
    #[test]
    fn a_roomless_request_exports_nothing_and_names_our_rooms() {
        let (host_tree, a, b) = host_with_two_rooms();
        let room_a = host_tree.room_of(a).expect("room A");
        let room_b = host_tree.room_of(b).expect("room B");
        let mut host = WorkspaceTreeReplica::new(1, host_tree.shared());

        host.handle(
            &PeerOrigin::hub(9),
            &NeighbourMsg::SyncReq {
                since: Vec::new(),
                room: String::new(),
            },
        );

        let out = host.drain_outgoing();
        assert!(
            out.iter()
                .all(|m| matches!(m, NeighbourMsg::SyncReq { .. })),
            "a roomless request must never be answered with a document: {out:?}"
        );
        let named: Vec<String> = out
            .iter()
            .filter_map(|m| match m {
                NeighbourMsg::SyncReq { room, .. } => Some(room.clone()),
                _ => None,
            })
            .collect();
        assert!(named.contains(&room_a.to_string()));
        assert!(named.contains(&room_b.to_string()));
        assert_eq!(named.len(), 2, "one request per room, no more: {named:?}");
    }

    /// The write-authority check on the live message path.
    ///
    /// A guest in room A publishes a `SyncResp` for room A carrying its own
    /// owner and its own grant. The host administers room A, so what it
    /// holds afterwards must be what it granted — read off the host's own
    /// state, through `handle`, which is the entry point the network actor
    /// calls.
    #[test]
    fn a_syncresp_that_would_rewrite_our_rooms_governance_is_not_taken() {
        const ROGUE_OWNER: &str = "guest-claiming-room-a";
        const ROGUE_PATH: &str = "/srv/rogue-claim";

        let (host_tree, a, _b) = host_with_two_rooms();
        let room_a = host_tree.room_of(a).expect("room A");
        let host_handle = host_tree.shared();
        let mut host = WorkspaceTreeReplica::new(1, host_handle.clone());

        // A guest in room A whose own state says it governs the room, and
        // which therefore projects its owner and its grants into it.
        let mut guest = WorkspaceTree::default();
        let guest_ws = guest.append_empty_workspace("Mine");
        assert!(guest.set_room(guest_ws, room_a), "the guest joins room A");
        guest
            .room_authority
            .insert(room_a, crate::shell::state::RoomAuthority::Local);
        guest
            .workspaces
            .iter_mut()
            .find(|w| w.id == guest_ws)
            .expect("the guest's workspace")
            .set_owner(ROGUE_OWNER.to_string());
        guest
            .grant_in_workspace(
                guest_ws,
                &ROGUE_OWNER.to_string(),
                CapOverride::grant(
                    Subject::app("rogue"),
                    Scope::FsPath(ROGUE_PATH.to_string()),
                    Verb::Write,
                ),
            )
            .expect("the guest grants in its own copy");
        let changes = guest
            .export_room_changes_since(room_a, &[])
            .expect("the guest is in room A");

        host.handle(
            &PeerOrigin::hub(9),
            &NeighbourMsg::SyncResp {
                changes,
                room: room_a.to_string(),
            },
        );

        let mut tree = host_handle.write();
        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == a)
            .expect("the host keeps room A's workspace");
        assert_eq!(
            ws.owner.as_deref(),
            Some(ROOM_A_OWNER),
            "a guest's SyncResp took ownership of the host's room",
        );
        assert!(
            ws.capabilities
                .iter()
                .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROOM_A_PATH)),
            "the owner's own grant was replaced by the guest's list",
        );
        assert!(
            !ws.capabilities
                .iter()
                .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROGUE_PATH)),
            "the guest's grant entered the host's capability list",
        );
        // And nowhere in what the host now holds — including the room
        // document it will hand to the next peer that asks.
        let held = holdings(&mut tree);
        assert!(
            !held.contains(ROGUE_OWNER) && !held.contains(ROGUE_PATH),
            "the refused governance is still somewhere in the host's state: \
             {held}"
        );
    }

    /// A response is merged into the room its envelope names, and the
    /// payload has to agree. A host that answered a request for room A
    /// with room B's document must not have that document accepted.
    #[test]
    fn a_response_whose_payload_names_another_room_is_refused() {
        let (mut host_tree, _a, b) = host_with_two_rooms();
        let room_b = host_tree.room_of(b).expect("room B");
        let room_b_changes = host_tree
            .export_room_changes_since(room_b, &[])
            .expect("the host is in room B");

        // The guest is in a room of its own and asked about that room.
        let mut guest_tree = WorkspaceTree::default();
        let guest_ws = guest_tree.append_empty_workspace("Mine");
        let guest_room = guest_tree.room_of(guest_ws).expect("the guest's room");

        let applied = guest_tree
            .apply_room_changes(guest_room, &room_b_changes, &PeerOrigin::hub(1))
            .expect("well-formed change blobs still parse");
        assert!(
            !applied,
            "a payload that names room B must not be merged into room {guest_room}",
        );
        let held = holdings(&mut guest_tree);
        assert!(
            !held.contains(ROOM_B_NAME) && !held.contains(ROOM_B_PATH),
            "the refused payload must not have been stored: {held}"
        );
    }

    /// Two replicas in the same room converge on that room's state after
    /// one round through the hub — the general convergence property the
    /// per-room path has to keep.
    #[test]
    fn two_replicas_converge_after_one_round_via_hub() {
        let (host_tree, a, _b) = host_with_two_rooms();
        let room_a = host_tree.room_of(a).expect("room A");

        let mut guest_tree = WorkspaceTree::default();
        let guest_ws = guest_tree.append_empty_workspace("Placeholder");
        assert!(guest_tree.set_room(guest_ws, room_a), "the guest joins room A");

        let tree_a = host_tree.shared();
        let tree_b = guest_tree.shared();

        // Heads diverge before the round: A has a document for the room,
        // B has none. A's is projected lazily, so force it here — the
        // precondition is about state, not about when the projection runs.
        tree_a.write().sync_room_doc(room_a);
        assert!(
            !tree_a.write().room_heads(room_a).is_empty(),
            "A must have something to ship for room A"
        );
        assert!(
            tree_b.write().room_heads(room_a).is_empty(),
            "B starts with no copy of room A"
        );

        let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree_a.clone())));
        let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, tree_b.clone())));

        let mut hub = Hub::new();
        hub.attach(Box::new(ArcReplica(rep_a.clone())));
        hub.attach(Box::new(ArcReplica(rep_b.clone())));
        sync_round(&mut hub, &rep_b, &[&rep_a]);

        assert_eq!(
            tree_a.write().room_heads(room_a),
            tree_b.write().room_heads(room_a),
            "after one hub round, A and B must hold the same room document"
        );
        let guest = tree_b.read();
        let ws = guest
            .workspaces
            .iter()
            .find(|w| w.id == guest_ws)
            .expect("the guest keeps its own workspace");
        assert_eq!(ws.name, ROOM_A_NAME, "the room's name arrived");
        assert!(
            ws.capabilities
                .iter()
                .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROOM_A_PATH)),
            "the room's capabilities arrived"
        );
    }

    /// What arrived has to be *announceable*, not just present.
    ///
    /// The guest's tree holding a peer's changes is what every existing
    /// assertion here checks, and it is not the same as the guest
    /// seeing them: the shell splices its widget tree only when the
    /// chrome revision moves, and this ingest runs on the network
    /// thread, which cannot touch the chrome. So the tree leaves a mark
    /// the frame loop reads instead. Without it a replicated window sat
    /// in the guest's tree — readable, assertable, provably there — and
    /// his screen stayed empty, which is a failure that passes every
    /// byte-level test in this file.
    #[test]
    fn a_round_that_lands_content_is_announced_to_the_frame_loop() {
        let (host_tree, a, _b) = host_with_two_rooms();
        let room_a = host_tree.room_of(a).expect("room A");

        let mut guest_tree = WorkspaceTree::default();
        let guest_ws = guest_tree.append_empty_workspace("Placeholder");
        assert!(guest_tree.set_room(guest_ws, room_a), "the guest joins room A");

        let tree_a = host_tree.shared();
        let tree_b = guest_tree.shared();
        tree_a.write().sync_room_doc(room_a);

        let before = tree_b.read().room_epoch();

        let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree_a.clone())));
        let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, tree_b.clone())));
        let mut hub = Hub::new();
        hub.attach(Box::new(ArcReplica(rep_a.clone())));
        hub.attach(Box::new(ArcReplica(rep_b.clone())));
        sync_round(&mut hub, &rep_b, &[&rep_a]);

        // Precondition: the round actually delivered something. Without
        // this the epoch assertion below could pass vacuously on a
        // round that carried nothing.
        assert_eq!(
            tree_a.write().room_heads(room_a),
            tree_b.write().room_heads(room_a),
            "the round must have delivered the room document"
        );
        assert!(
            tree_b.read().room_epoch() > before,
            "content landed in the guest's tree but nothing told the frame \
             loop to rebuild, so it would never be drawn"
        );
    }

    /// And a round that delivered nothing must not claim otherwise —
    /// an epoch that ticks on every quiet sync would repaint the whole
    /// shell at gossip rate and make the signal useless.
    #[test]
    fn a_round_that_lands_nothing_is_not_announced() {
        let (host_tree, a, _b) = host_with_two_rooms();
        let room_a = host_tree.room_of(a).expect("room A");
        let mut guest_tree = WorkspaceTree::default();
        let guest_ws = guest_tree.append_empty_workspace("Placeholder");
        assert!(guest_tree.set_room(guest_ws, room_a));

        let tree_a = host_tree.shared();
        let tree_b = guest_tree.shared();
        tree_a.write().sync_room_doc(room_a);

        let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree_a.clone())));
        let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, tree_b.clone())));
        let mut hub = Hub::new();
        hub.attach(Box::new(ArcReplica(rep_a.clone())));
        hub.attach(Box::new(ArcReplica(rep_b.clone())));
        sync_round(&mut hub, &rep_b, &[&rep_a]);
        let settled = tree_b.read().room_epoch();

        // Nothing changed on either side; sync again.
        sync_round(&mut hub, &rep_b, &[&rep_a]);
        assert_eq!(
            tree_b.read().room_epoch(),
            settled,
            "a sync round that carried no change must not announce one"
        );
    }

    #[test]
    fn drain_outgoing_returns_fifo_and_clears() {
        let tree = WorkspaceTree::default().shared();
        let rep = WorkspaceTreeReplica::new(42, tree);

        rep.pending_outgoing
            .lock()
            .unwrap()
            .push(NeighbourMsg::SyncReq {
                since: vec![1],
                room: String::new(),
            });
        rep.pending_outgoing
            .lock()
            .unwrap()
            .push(NeighbourMsg::SyncReq {
                since: vec![2],
                room: String::new(),
            });

        let drained = rep.drain_outgoing();
        assert_eq!(drained.len(), 2);
        match &drained[0] {
            NeighbourMsg::SyncReq { since, .. } => assert_eq!(since, &vec![1]),
            other => panic!("expected SyncReq, got {:?}", other),
        }
        match &drained[1] {
            NeighbourMsg::SyncReq { since, .. } => assert_eq!(since, &vec![2]),
            other => panic!("expected SyncReq, got {:?}", other),
        }
        assert!(
            rep.drain_outgoing().is_empty(),
            "drain must clear the queue"
        );
    }

    /// D-8 regression: `request_sync` serialises the local heads via
    /// `serde_json::to_vec`. The current code uses `unwrap_or_default()`
    /// which silently turns a serialise failure into a "since: []"
    /// full-sync request for that room — wasteful but not catastrophic,
    /// and it cannot widen past the room it names. This pins the success
    /// path so a refactor that swaps the error handling can't accidentally
    /// regress to a panic on finite payloads.
    ///
    /// We do not test the failure mode because `Vec<ChangeHash>` has no
    /// realistic way to fail `serde_json::to_vec`; the silent-error path
    /// is documented in the production code instead.
    #[test]
    fn request_sync_unwrap_is_safe_for_finite_payloads() {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Only room");
        let room = tree.room_of(ws).expect("a fresh workspace has a room");
        let rep = WorkspaceTreeReplica::new(99, tree.shared());

        // The success path must not panic.
        rep.request_sync();

        let queued = rep.drain_outgoing();
        assert_eq!(queued.len(), 1, "one SyncReq per room this machine is in");
        match &queued[0] {
            NeighbourMsg::SyncReq { since, room: r } => {
                // Empty heads serialises to "[]" (2 bytes), not an empty vec.
                assert!(
                    !since.is_empty(),
                    "encoded `since` must be a valid JSON array, got empty vec"
                );
                assert_eq!(r, &room.to_string(), "the request names the room");
            }
            other => panic!("expected SyncReq, got {:?}", other),
        }
    }

    /// A machine in no room asks for nothing — there is nothing it could
    /// be caught up on, and a request naming no room is not a request for
    /// everything.
    #[test]
    fn a_machine_in_no_room_requests_nothing() {
        let rep = WorkspaceTreeReplica::new(99, WorkspaceTree::default().shared());
        rep.request_sync();
        assert!(rep.drain_outgoing().is_empty());
    }

    /// C-9 regression: when the merge fails (a malformed `SyncResp` from a
    /// peer, e.g.), the previous code dropped the `Result` on the floor.
    /// The replica believed the sync round succeeded while the document
    /// actually remained un-mutated. Verifies that `handle()` still
    /// returns cleanly (doesn't panic, doesn't propagate) on bogus
    /// change-bytes — the production code now logs at `tracing::error!`,
    /// but the observable side effect at this test layer is "the function
    /// returns instead of corrupting the document".
    #[test]
    fn apply_changes_failure_is_logged() {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Mine");
        let room = tree.room_of(ws).expect("room");
        let handle = tree.shared();
        let mut rep = WorkspaceTreeReplica::new(11, handle.clone());

        // Snapshot pre-state so we can prove the room was *not* mutated by
        // the failed apply.
        let heads_before = handle.write().room_heads(room);

        // Bogus byte vec — not a valid automerge change blob.
        // `Change::try_from(&[..])` will fail before any mutation lands,
        // causing `apply_room_changes` to return `Err`.
        let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF];
        rep.handle(
            &PeerOrigin::hub(7),
            &NeighbourMsg::SyncResp {
                changes: vec![garbage],
                room: room.to_string(),
            },
        );

        // The function must NOT have panicked (we got here). The room must
        // NOT have been mutated by the failed apply — heads stay identical.
        let heads_after = handle.write().room_heads(room);
        assert_eq!(
            heads_before, heads_after,
            "apply failed but the room's heads changed — fix allowed partial \
             application?"
        );
        // No outbound traffic queued either: SyncResp doesn't reply.
        assert!(
            rep.pending_outgoing.lock().unwrap().is_empty(),
            "failed SyncResp must not enqueue outbound traffic"
        );
    }

    #[test]
    fn unhandled_message_variants_are_ignored() {
        let tree = WorkspaceTree::default().shared();
        let mut rep = WorkspaceTreeReplica::new(7, tree);

        rep.handle(
            &PeerOrigin::hub(1),
            &NeighbourMsg::PresenceCleared {
                peer: 1,
                key: "cursor".into(),
            },
        );
        assert!(
            rep.pending_outgoing.lock().unwrap().is_empty(),
            "non-sync messages must not enqueue any outbound traffic"
        );
    }

    /// C-3 Phase D integration test.
    ///
    /// A workspace appended via the shell chrome's `add_workspace` (or any
    /// mutating helper that goes through `tree_mut()`) must be visible to
    /// the peer-sync replica's export — they share the same
    /// `WorkspaceTreeHandle`, so the mutation reaches the room the peer
    /// asks about. Before this refactor the chrome and the replica held
    /// independent trees and the shell mutation never reached peers.
    ///
    /// Closes the second observable consequence in
    /// `docs/superpowers/specs/2026-05-22-workspace-tree-arc-mutex-design.md`.
    #[test]
    fn workspace_added_via_chrome_is_visible_in_peer_sync_export() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTreeHandleExt;

        // Construct via the handle so chrome + replica share the same
        // `Arc<RwLock<WorkspaceTree>>`. The handle starts empty so the
        // first chrome mutation drives every subsequent change.
        let handle: WorkspaceTreeHandle = WorkspaceTreeHandle::test_default();
        let replica = WorkspaceTreeReplica::new(42, handle.clone());
        let chrome = ChromeState::new_with_handle(handle.clone());

        // Shell-side mutation: append a workspace through the chrome
        // helper. This is the production path the wheel-disc's "+" slot
        // hits, and it mints the new workspace's room.
        chrome.borrow_mut().add_workspace("New");
        let room = {
            let tree = handle.read();
            tree.workspaces
                .iter()
                .find(|w| w.name == "New")
                .map(|w| w.room)
                .expect("the chrome's append mints a room")
        };

        let changes = replica
            .tree
            .write()
            .export_room_changes_since(room, &[])
            .expect("the tree is in the room the chrome just made");
        assert!(
            !changes.is_empty(),
            "shell-side add_workspace must produce automerge changes the \
             peer-sync replica can ship to peers in that room (audit C-3: \
             shared WorkspaceTreeHandle ties subsystems together)",
        );
    }
}
