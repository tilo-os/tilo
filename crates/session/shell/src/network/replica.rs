//! Glue between the in-process [`Hub`] and the [`WorkspaceTree`]'s automerge
//! sync API: a [`WorkspaceTreeReplica`] wraps a tree + peer id + a queue of
//! outbound deliveries, and implements [`TopicSubscriber`] so a [`Hub`] can
//! fan messages to it.
//!
//! Replication is per room — a peer invited to one workspace gets nothing
//! of any other, since `WorkspaceTree::export_room_changes_since` never
//! exports anything else in the first place.
//!
//! Protocol: `SyncReq{since,room}` for a room we're in → `SyncResp` with
//! that room's changes, queued in `pending_outgoing` for
//! [`Hub::broadcast`]. `SyncReq` for a room we're not in → no reply, not
//! even that it's unknown. `SyncReq` naming no room (the transport's
//! peer-join kick) → exports nothing, instead requests each of our own
//! rooms individually. `SyncResp{changes,room}` →
//! `tree.apply_room_changes`, which refuses a room we're not in before
//! storing anything. Other variants are ignored here.
//!
//! [`Hub`]: crate::network::hub::Hub
//! [`Hub::broadcast`]: crate::network::hub::Hub::broadcast
//! [`TopicSubscriber`]: crate::network::hub::TopicSubscriber
//! [`WorkspaceTree`]: crate::shell::state::WorkspaceTree

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::network::hub::TopicSubscriber;
use crate::network::protocol::NeighbourMsg;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
use crate::shell::state::WorkspaceTreeHandle;
use crate::shell::state::{PeerOrigin, RoomId};

/// Replica-side glue: a (shared) `WorkspaceTree` **handle**, a peer id,
/// and a FIFO queue of messages to broadcast on its behalf.
///
/// The tree is a [`WorkspaceTreeHandle`]
/// (one tree behind a lock, with a write generation) shared with the shell chrome
/// and the portal's grant store, so `apply_changes` is visible to both
/// immediately. `pending_outgoing` uses `std::sync::Mutex` — drained from
/// arbitrary threads, no read-heavy workload to motivate `RwLock`.
///
/// [`Hub`]: crate::network::hub::Hub
pub struct WorkspaceTreeReplica {
    pub id: u64,
    pub tree: WorkspaceTreeHandle,
    pub pending_outgoing: Arc<Mutex<Vec<NeighbourMsg>>>,
    /// Transport identity this replica's messages arrive under — only
    /// meaningful when driven through a [`Hub`]; live wiring calls
    /// [`Self::handle_from`] with the authenticated `PeerId` directly instead.
    ///
    /// [`Hub`]: crate::network::hub::Hub
    pub origin: PeerOrigin,
    /// Who has actually *joined* each room's document, distinct from who was
    /// invited — a peer that asks this room for its changes, or answers with
    /// them, is a participant. Keyed off [`PeerOrigin::liveness_handle`], the
    /// transport-authenticated identity rather than anything the message
    /// claimed, so a peer can't put itself in here by saying it is here.
    /// Session-scoped, never persisted.
    participants: std::collections::HashMap<RoomId, std::collections::BTreeSet<u64>>,
    /// Advanced each time a peer is first seen taking part in a room, so the
    /// roster can tell that nobody new arrived without locking the replica.
    participants_epoch: Arc<AtomicU64>,
    /// The heads a peer's request named that this replica asked a room back
    /// for, and when (see [`PULL_BACK_REPEAT`]).
    pulled_back: std::collections::HashMap<RoomId, (Vec<automerge::ChangeHash>, Instant)>,
}

/// How long a replica waits before asking a room back again for the same
/// heads it lacked. Changes this machine refuses stay unknown here however
/// often they are offered; two peers that each refuse the other's would
/// otherwise ask each other back forever. New heads are asked for at once.
const PULL_BACK_REPEAT: Duration = Duration::from_secs(300);

impl WorkspaceTreeReplica {
    pub fn new(id: u64, tree: WorkspaceTreeHandle) -> Self {
        Self {
            id,
            tree,
            pending_outgoing: Arc::new(Mutex::new(Vec::new())),
            origin: PeerOrigin::hub(id),
            participants: std::collections::HashMap::new(),
            participants_epoch: Arc::default(),
            pulled_back: std::collections::HashMap::new(),
        }
    }

    /// Whether `room` may be asked back now for the `heads` a peer named,
    /// noting that it is.
    fn pull_back_due(&mut self, room: RoomId, heads: &[automerge::ChangeHash]) -> bool {
        let now = Instant::now();
        if self.pulled_back.get(&room).is_some_and(|(asked, at)| {
            asked.as_slice() == heads && now.saturating_duration_since(*at) < PULL_BACK_REPEAT
        }) {
            return false;
        }
        self.pulled_back.insert(room, (heads.to_vec(), now));
        true
    }

    /// Record that `from` has taken part in `room`'s document; a no-op for an
    /// origin with no peer id (in-process hub subscriber or local replay).
    fn note_participant(&mut self, room: RoomId, from: &PeerOrigin) {
        let Some(handle) = from.liveness_handle() else {
            return;
        };
        if self.participants.entry(room).or_default().insert(handle) {
            self.participants_epoch.fetch_add(1, Ordering::Release);
        }
    }

    /// A counter that advances whenever [`Self::participants_in`] gains a
    /// peer in any room. Shared, so a reader holding it can compare two
    /// loads without taking the replica's lock.
    pub fn participants_epoch(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.participants_epoch)
    }

    /// The peers that have taken part in `room`'s document this session.
    pub fn participants_in(&self, room: RoomId) -> std::collections::BTreeSet<u64> {
        self.participants.get(&room).cloned().unwrap_or_default()
    }

    /// Deliver this replica's messages under `origin`.
    #[must_use]
    pub fn with_origin(mut self, origin: PeerOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Stash one `SyncReq` per room this machine is in, each carrying that
    /// room's current heads — asked room by room so an answer can't merge
    /// several rooms into one undifferentiated document.
    pub fn request_sync(&self) {
        let rooms = self.tree.read().rooms();
        self.request_sync_of(&rooms);
    }

    /// [`Self::request_sync`] for `rooms` alone.
    fn request_sync_of(&self, rooms: &[RoomId]) {
        let mut tree = self.tree.write();
        let mut msgs = Vec::with_capacity(rooms.len());
        for &room in rooms {
            if tree.requires_native_owner(room) {
                tracing::debug!(
                    target: "shell::network::replica",
                    %room,
                    "not requesting room replication: the room's native owner replicates it"
                );
                continue;
            }
            let heads = tree.room_heads(room);
            // Structurally infallible (`ChangeHash` has no enum/float to fail on);
            // `unwrap_or_default()`'s `vec![]` fallback reads as "send everything in
            // this room" — wasteful, not corrupting, and can't widen past the room it names.
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
                invite_id: None,
                nonce: getrandom::rand_core::Rng::next_u64(&mut getrandom::rand_core::UnwrapErr(
                    getrandom::SysRng,
                )),
            });
        }
        drop(tree);
        self.pending_outgoing.lock_recover().extend(msgs);
    }

    /// Drain pending outbound messages so a test (or Hub-driver) can broadcast them; stable FIFO order.
    pub fn drain_outgoing(&self) -> Vec<NeighbourMsg> {
        std::mem::take(&mut *self.pending_outgoing.lock_recover())
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
    /// authenticated, which an authority decision may rest on: the
    /// room-replication path decides whether a payload may rewrite a room's
    /// governance based on **who** sent it, and the hub's `u64` id (a
    /// `DefaultHasher` digest in production) isn't safe for that. Live wiring
    /// calls this with the peer's own identity ([`PeerOrigin::peer`]).
    pub fn handle_from(&mut self, from: &PeerOrigin, msg: &NeighbourMsg) {
        let sender = from;
        if let NeighbourMsg::SyncReq { room, .. } | NeighbourMsg::SyncResp { room, .. } = msg
            && RoomId::parse(room).is_some_and(|room| self.tree.read().requires_native_owner(room))
        {
            return;
        }
        match msg {
            NeighbourMsg::SyncReq { since, room, .. } => {
                // A roomless request is the transport's peer-join kick, not a request for
                // everything — answering with a document would be the leak this closes.
                let Some(room_id) = RoomId::parse(room).filter(|r| !r.is_nil()) else {
                    self.request_sync();
                    return;
                };
                let since_heads: Vec<automerge::ChangeHash> =
                    serde_json::from_slice(since).unwrap_or_default();
                let mut tree = self.tree.write();
                let exported = tree.export_room_changes_since(room_id, &since_heads);
                let behind = exported.is_some() && tree.room_lacks_any(room_id, &since_heads);
                drop(tree);
                // `None` = not in that room. No reply — a peer learns nothing about a room it named, not even that it's unknown here.
                let Some(changes) = exported else {
                    return;
                };
                // Recorded after "not in that room" (so asking about an absent room
                // doesn't count) and before "nothing to send" (an up-to-date guest still counts as a participant).
                self.note_participant(room_id, sender);
                // The asker holds changes this machine does not: a push of
                // them never arrived here. Ask back rather than wait for this
                // machine's own next pull, which an idle desktop makes only
                // every few minutes; the asker's pull after its own activity
                // is what repairs a push its peers missed.
                if behind && self.pull_back_due(room_id, &since_heads) {
                    self.request_sync_of(&[room_id]);
                }
                if changes.is_empty() {
                    return;
                }
                self.pending_outgoing
                    .lock_recover()
                    .push(NeighbourMsg::SyncResp {
                        changes,
                        room: room_id.to_string(),
                    });
            }
            NeighbourMsg::SyncResp { changes, room } => {
                // Failures log via `tracing::error!` rather than propagate; the connection
                // stays alive. `apply_room_changes` refuses a room we're not in before
                // storing anything, then folds an accepted room into the shared tree.
                let Some(room_id) = RoomId::parse(room).filter(|r| !r.is_nil()) else {
                    tracing::warn!(
                        target: "shell::network::replica",
                        sender = %sender,
                        "SyncResp names no room; dropped",
                    );
                    return;
                };
                let result = self.tree.write().apply_room_changes(room_id, changes, from);
                if let Err(err) = result {
                    tracing::error!(
                        target: "shell::network::replica",
                        sender = %sender,
                        room = %room_id,
                        change_count = changes.len(),
                        ?err,
                        "WorkspaceTreeReplica::apply_room_changes failed; \
                         peer-sync round produced no doc mutation (C-9)",
                    );
                    return;
                }
                // Recorded only once accepted — a refused document must not leave a participant behind it.
                self.note_participant(room_id, sender);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    //! Two-replica convergence over a [`Hub`]. The hub owns its subscribers, so
    //! to keep a handle after attach we wrap each replica in `Arc<Mutex<_>>`
    //! and attach a thin proxy forwarding `TopicSubscriber` calls, the same
    //! pattern `hub.rs`'s tests use.
    //!
    //! [`Hub`]: crate::network::hub::Hub
    use std::fmt::Write as _;

    use super::*;
    use crate::network::hub::Hub;
    use automerge::{ROOT, ReadDoc, ScalarValue};

    /// Proxy that forwards `TopicSubscriber` calls into a shared replica
    /// behind `Arc<Mutex<_>>` — mirrors `hub.rs`'s `ArcSub` test helper.
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

    // The claim under test: a peer invited to room A holds nothing of room B.
    // `holdings` walks the guest's cache, tree document (materialised heads
    // and raw change bytes), and every room document for room B's name, id, owner, or capability.

    use crate::network::invite::{accept_invite_into, make_invite_token};
    use crate::shell::state::WorkspaceId;
    use collab_types::caps::{Access, CapOverride, Scope, Subject};

    /// Room A's distinguishing strings, chosen so a substring search can't match them by accident.
    const ROOM_A_NAME: &str = "Design review";
    const ROOM_A_OWNER: &str = "alice-owns-room-a";
    const ROOM_A_PATH: &str = "/home/quinten/room-a-notes";
    /// Room B's. Nothing in this list may ever reach the guest.
    const ROOM_B_NAME: &str = "Payroll";
    const ROOM_B_OWNER: &str = "bob-owns-room-b";
    const ROOM_B_PATH: &str = "/srv/room-b-secrets";

    /// A host with two rooms, one it invites a guest into and one it doesn't,
    /// both fully furnished — so "nothing of room B" has something to be false about.
    fn host_with_two_rooms() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let mut tree = WorkspaceTree::default();
        let a = tree.append_empty_workspace(ROOM_A_NAME);
        let b = tree.append_empty_workspace(ROOM_B_NAME);

        // Re-point each room at its own owner so the two are distinguishable on the wire.
        tree.adopt_ownerless_workspaces(&ROOM_A_OWNER.to_string());
        if let Some(ws) = tree.desktop.workspaces.iter_mut().find(|w| w.id == b) {
            ws.set_owner(ROOM_B_OWNER.to_string());
        }

        tree.grant_sandbox_in_workspace(
            a,
            &ROOM_A_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(ROOM_A_PATH.to_string()),
                Access::Read,
            ),
        )
        .expect("the owner may grant in room A");
        tree.grant_sandbox_in_workspace(
            b,
            &ROOM_B_OWNER.to_string(),
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath(ROOM_B_PATH.to_string()),
                Access::Read,
            ),
        )
        .expect("the owner may grant in room B");
        tree.seed_doc_from_cache();
        (tree, a, b)
    }

    /// Everything a tree holds, flattened into one searchable string: the
    /// cache, the tree document (current heads and every change's raw bytes), and every room document.
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
                // `String`'s `Write` impl is infallible.
                other => {
                    let _ = write!(out, "{other:?}");
                }
            },
            Value::Object(ObjType::List | ObjType::Text) => {
                for i in 0..doc.length(id) {
                    if let Some((v, child)) = doc.get(id, i).ok().flatten() {
                        dump_value(doc, &v, &child, out);
                    }
                }
            }
            Value::Object(_) => dump_map(doc, id, out),
        }
    }

    /// Run one full sync round between two replicas over a [`Hub`]: `initiator` asks, the other answers, the answer comes back.
    fn sync_round(
        hub: &mut Hub,
        initiator: &Arc<Mutex<WorkspaceTreeReplica>>,
        others: &[&Arc<Mutex<WorkspaceTreeReplica>>],
    ) {
        // Queues are drained into a local binding before the hub is pumped —
        // `Hub::broadcast` asks each subscriber for its id, which would re-enter a still-live `lock()` temporary's mutex.
        initiator.lock().unwrap().request_sync();
        let from = initiator.lock().unwrap().id();
        let outbound = initiator.lock().unwrap().drain_outgoing();
        for msg in &outbound {
            hub.broadcast(from, msg);
        }
        for other in others {
            let id = other.lock().unwrap().id();
            let replies = other.lock().unwrap().drain_outgoing();
            for msg in &replies {
                hub.broadcast(id, msg);
            }
        }
    }

    #[test]
    fn rejected_replies_do_not_commit_a_pending_local_rename() {
        use automerge::transaction::Transactable as _;

        let (mut tree, workspace, _) = host_with_two_rooms();
        let room = tree.room_of(workspace).expect("room A");
        tree.sync_room_doc(room);
        let original_heads = tree.room_heads(room);
        let mut rogue = tree.room_docs.get(&room).expect("room document").clone();
        rogue
            .put(automerge::ROOT, "owner", "rogue-owner")
            .expect("rogue change");
        let rogue_changes = rogue
            .get_changes(&original_heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace)
            .expect("workspace")
            .name = "Renamed locally".into();
        tree.seed_doc_from_cache();
        let tree = tree.shared();
        let mut replica = WorkspaceTreeReplica::new(1, tree.clone());
        for changes in [rogue_changes, vec![vec![0xff]]] {
            replica.handle_from(
                &PeerOrigin::hub(2),
                &NeighbourMsg::SyncResp {
                    changes,
                    room: room.to_string(),
                },
            );
            let mut tree = tree.write();
            assert_eq!(tree.room_heads(room), original_heads);
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == workspace)
                .expect("workspace");
            let name = workspace.name.clone();
            let owner = workspace.owner().cloned();
            drop(tree);
            assert_eq!(name, "Renamed locally");
            assert_eq!(owner.as_deref(), Some(ROOM_A_OWNER));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn room_scoped_sync_exports_nothing_outside_our_rooms() {
        // Phase `a_guest_invited_to_one_room_holds_nothing_of_another`.
        // **The negative.** A guest invited to room A must hold nothing of room
        // B. The invite is minted/accepted through the production path
        // (`make_invite_token`/`accept_invite_into`), the round runs through
        // `WorkspaceTreeReplica::handle` over a `Hub`, and assertions read the guest's own state afterwards.
        {
            let (host_tree, host_a, host_b) = host_with_two_rooms();
            let room_a = host_tree.room_of(host_a).expect("room A");
            let room_b = host_tree.room_of(host_b).expect("room B");

            // The guest: its own machine, its own workspace, its own room — until it accepts the invite.
            let mut guest_tree = WorkspaceTree::default();
            let guest_ws = guest_tree.append_empty_workspace("My workspace");

            let owner_key = iroh::SecretKey::generate();
            let owner_addr = iroh::EndpointAddr::new(owner_key.public())
                .with_ip_addr("127.0.0.1:4001".parse().unwrap());
            let token = make_invite_token(
                &owner_key,
                host_tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == host_a)
                    .expect("host workspace A"),
                &owner_addr,
                3600,
            )
            .expect("the host issues an invite to room A");
            let joined = accept_invite_into(&mut guest_tree, guest_ws, &token)
                .expect("the guest accepts it");
            assert_eq!(joined, room_a, "the invite names room A");

            // The host replica delivers under the identity its own invite named — a
            // guest takes a room's owner/capabilities from the peer whose signature admitted it.
            let host = Arc::new(Mutex::new(
                WorkspaceTreeReplica::new(1, host_tree.shared())
                    .with_origin(PeerOrigin::peer(&owner_key.public())),
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

            // Asserted first, so a regression is reported as the field that leaked rather than a downstream symptom.
            assert_absent(&held, ROOM_B_NAME, "room B's name");
            assert_absent(&held, &room_b.to_string(), "room B's room id");
            assert_absent(&held, ROOM_B_OWNER, "room B's owner");
            assert_absent(&held, ROOM_B_PATH, "room B's capability");
            // And the guest gained no workspace it was not invited to.
            assert_eq!(
                guest_handle.read().desktop.workspaces.len(),
                1,
                "the guest has one workspace — its own, now in room A",
            );

            // Positive control — without it, a guest that received nothing at all would satisfy every assertion above.
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
                    .desktop
                    .workspaces
                    .iter()
                    .find(|w| w.id == guest_ws)
                    .map(|w| w.name.clone()),
                Some(ROOM_A_NAME.to_string()),
                "the round must land in the guest's own workspace, not beside it",
            );
            let _ = room_a;
        }
        // Phase `a_request_for_a_room_we_are_not_in_is_not_answered`.
        // The other direction: a request naming a room this machine is not in gets nothing back, not whatever the machine happens to have.
        {
            let (host_tree, _a, _b) = host_with_two_rooms();
            let stranger_room = crate::shell::state::RoomId::new();
            let mut host = WorkspaceTreeReplica::new(1, host_tree.shared());

            host.handle(
                &PeerOrigin::hub(9),
                &NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new()).unwrap(),
                    room: stranger_room.to_string(),
                    invite_id: None,
                    nonce: 0,
                },
            );
            assert!(
                host.drain_outgoing().is_empty(),
                "a room we are not in must produce no reply — not an empty one, \
                 which would still confirm the request was understood",
            );
        }
        // Phase `a_roomless_request_exports_nothing_and_names_our_rooms`.
        // The transport's peer-join kick names no room; it must not be read as
        // "send everything" — it exports nothing and makes this replica request its own rooms individually.
        {
            let (host_tree, a, b) = host_with_two_rooms();
            let room_a = host_tree.room_of(a).expect("room A");
            let room_b = host_tree.room_of(b).expect("room B");
            let mut host = WorkspaceTreeReplica::new(1, host_tree.shared());

            host.handle(
                &PeerOrigin::hub(9),
                &NeighbourMsg::SyncReq {
                    since: Vec::new(),
                    room: String::new(),
                    invite_id: None,
                    nonce: 0,
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
    }

    /// Fail naming the leaked field, with the surrounding bytes of the guest's
    /// own state as evidence — a whole dump would bury which assertion tripped.
    fn assert_absent(held: &str, needle: &str, what: &str) {
        let Some(at) = held.find(needle) else {
            return;
        };
        let start = held[..at]
            .char_indices()
            .rev()
            .nth(70)
            .map_or(0, |(i, _)| i);
        let end = held[at..]
            .char_indices()
            .nth(needle.chars().count().saturating_add(70))
            .map_or(held.len(), |(i, _)| at.saturating_add(i));
        panic!(
            "{what} ({needle:?}) leaked to a guest invited only to room A.\n\
             Found in the guest's own state at byte {at}:\n…{}…",
            &held[start..end]
        );
    }

    #[test]
    fn sync_responses_cannot_rewrite_or_cross_rooms() {
        // Phase `a_syncresp_that_would_rewrite_our_rooms_governance_is_not_taken`.
        // The write-authority check on the live path: a guest in room A
        // publishes a `SyncResp` carrying its own owner/grant; what the host holds afterwards must match what it granted.
        {
            const ROGUE_OWNER: &str = "guest-claiming-room-a";
            const ROGUE_PATH: &str = "/srv/rogue-claim";

            let (host_tree, a, _b) = host_with_two_rooms();
            let room_a = host_tree.room_of(a).expect("room A");
            let host_handle = host_tree.shared();
            let mut host = WorkspaceTreeReplica::new(1, host_handle.clone());

            // A guest whose own state says it governs room A, and which therefore projects its owner and grants into it.
            let mut guest = WorkspaceTree::default();
            let guest_ws = guest.append_empty_workspace("Mine");
            assert!(guest.set_room(guest_ws, room_a), "the guest joins room A");
            guest
                .room_authority
                .insert(room_a, crate::shell::state::RoomAuthority::Local);
            guest
                .desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == guest_ws)
                .expect("the guest's workspace")
                .set_owner(ROGUE_OWNER.to_string());
            guest
                .grant_sandbox_in_workspace(
                    guest_ws,
                    &ROGUE_OWNER.to_string(),
                    CapOverride::grant(
                        Subject::app("rogue"),
                        Scope::FsPath(ROGUE_PATH.to_string()),
                        Access::Write,
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
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == a)
                .expect("the host keeps room A's workspace");
            assert_eq!(
                ws.owner().map(String::as_str),
                Some(ROOM_A_OWNER),
                "a guest's SyncResp took ownership of the host's room",
            );
            assert!(
                ws.sandbox
                    .iter()
                    .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROOM_A_PATH)),
                "the owner's own grant was replaced by the guest's list",
            );
            assert!(
                !ws.sandbox
                    .iter()
                    .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROGUE_PATH)),
                "the guest's grant entered the host's capability list",
            );
            // Nowhere in what the host now holds — including the room document it will hand to the next peer that asks.
            let held = holdings(&mut tree);
            drop(tree);
            assert!(
                !held.contains(ROGUE_OWNER) && !held.contains(ROGUE_PATH),
                "the refused governance is still somewhere in the host's state: \
                 {held}"
            );
        }
        // Phase `a_response_whose_payload_names_another_room_is_refused`.
        // A response is merged into the room its envelope names, and the payload
        // has to agree — a host answering room A's request with room B's document must not have that accepted.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn replicas_converge_and_announce_only_rounds_that_land_content() {
        // Phase `two_replicas_converge_after_one_round_via_hub`.
        // Two replicas in the same room converge on that room's state after one round through the hub.
        {
            let (host_tree, a, _b) = host_with_two_rooms();
            let room_a = host_tree.room_of(a).expect("room A");

            let mut guest_tree = WorkspaceTree::default();
            let guest_ws = guest_tree.append_empty_workspace("Placeholder");
            assert!(
                guest_tree.set_room(guest_ws, room_a),
                "the guest joins room A"
            );
            // The invite names the host (hub 1) as room A's governor.
            assert!(guest_tree.pin_room_authority(room_a, PeerOrigin::hub(1)));

            let tree_a = host_tree.shared();
            let tree_b = guest_tree.shared();

            // Heads diverge before the round: A has a document for the room, B has
            // none. A's is projected lazily, so force it here — the precondition is about state, not when projection runs.
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
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == guest_ws)
                .expect("the guest keeps its own workspace");
            assert_eq!(ws.name, ROOM_A_NAME, "the room's name arrived");
            assert!(
                ws.sandbox
                    .iter()
                    .any(|c| matches!(&c.scope, Scope::FsPath(p) if p == ROOM_A_PATH)),
                "the room's capabilities arrived"
            );
            drop(guest);
        }
        // Phase `three_peers_converge_after_revocation_and_reconnect`.
        // Software-only multi-machine campaign: two collaborators join the same
        // authority, restart/reconnect independently, and converge on a revocation
        // without losing the other collaborator's role.
        {
            let (mut host_tree, host_workspace, _other) = host_with_two_rooms();
            let room = host_tree.room_of(host_workspace).expect("room A");
            let owner = ROOM_A_OWNER.to_string();
            let bob = "bob-device-root".to_string();
            let carol = "carol-device-root".to_string();
            host_tree
                .admit_in_workspace(
                    host_workspace,
                    &owner,
                    &bob,
                    tilo_action_policy::Role::Editor,
                )
                .expect("owner admits Bob");
            host_tree
                .admit_in_workspace(
                    host_workspace,
                    &owner,
                    &carol,
                    tilo_action_policy::Role::Commenter,
                )
                .expect("owner admits Carol");
            host_tree.sync_room_doc(room);

            let authority_key = iroh::SecretKey::generate();
            let guest = |name: &str| {
                let mut tree = WorkspaceTree::default();
                let workspace = tree.append_empty_workspace(name);
                assert!(tree.set_room(workspace, room));
                // Each accepted the authority's invite, which pins it.
                assert!(tree.pin_room_authority(room, PeerOrigin::peer(&authority_key.public())));
                (tree.shared(), workspace)
            };
            let (bob_tree, bob_workspace) = guest("Bob local");
            let (carol_tree, carol_workspace) = guest("Carol local");
            let host_tree = host_tree.shared();
            let host = Arc::new(Mutex::new(
                WorkspaceTreeReplica::new(1, host_tree.clone())
                    .with_origin(PeerOrigin::peer(&authority_key.public())),
            ));
            let bob_replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, bob_tree.clone())));
            let carol_replica =
                Arc::new(Mutex::new(WorkspaceTreeReplica::new(3, carol_tree.clone())));
            let mut hub = Hub::new();
            hub.attach(Box::new(ArcReplica(host.clone())));
            hub.attach(Box::new(ArcReplica(bob_replica.clone())));
            hub.attach(Box::new(ArcReplica(carol_replica.clone())));

            sync_round(&mut hub, &bob_replica, &[&host, &carol_replica]);
            sync_round(&mut hub, &carol_replica, &[&host, &bob_replica]);
            for (tree, workspace) in [(&bob_tree, bob_workspace), (&carol_tree, carol_workspace)] {
                let (bob_role, carol_role) = {
                    let tree = tree.read();
                    let workspace = tree
                        .desktop
                        .workspaces
                        .iter()
                        .find(|ws| ws.id == workspace)
                        .unwrap();
                    let roles = (workspace.role_of(&bob), workspace.role_of(&carol));
                    drop(tree);
                    roles
                };
                assert_eq!(bob_role, Some(crate::shell::state::CapRole::Editor));
                assert_eq!(carol_role, Some(crate::shell::state::CapRole::Commenter));
            }

            assert!(
                host_tree
                    .write()
                    .remove_from_workspace(host_workspace, &owner, &bob)
            );

            // Carol stays online; Bob's replica is reconstructed to model a
            // process restart before reconnecting with its persisted tree.
            sync_round(&mut hub, &carol_replica, &[&host, &bob_replica]);
            let restarted_bob =
                Arc::new(Mutex::new(WorkspaceTreeReplica::new(4, bob_tree.clone())));
            hub.attach(Box::new(ArcReplica(restarted_bob.clone())));
            sync_round(&mut hub, &restarted_bob, &[&host, &carol_replica]);

            for (tree, workspace) in [(&bob_tree, bob_workspace), (&carol_tree, carol_workspace)] {
                let (bob_role, carol_role) = {
                    let tree = tree.read();
                    let workspace = tree
                        .desktop
                        .workspaces
                        .iter()
                        .find(|ws| ws.id == workspace)
                        .unwrap();
                    let roles = (workspace.role_of(&bob), workspace.role_of(&carol));
                    drop(tree);
                    roles
                };
                assert_eq!(bob_role, None, "revoked Bob survived reconnect");
                assert_eq!(
                    carol_role,
                    Some(crate::shell::state::CapRole::Commenter),
                    "Bob's revocation damaged Carol's independent role"
                );
            }
        }
        // Phase `a_round_that_lands_content_is_announced_to_the_frame_loop`.
        // What arrived has to be *announceable*, not just present: the shell
        // splices its widget tree only when the chrome revision moves, and
        // ingest runs on the network thread, which can't touch the chrome directly.
        {
            let (host_tree, a, _b) = host_with_two_rooms();
            let room_a = host_tree.room_of(a).expect("room A");

            let mut guest_tree = WorkspaceTree::default();
            let guest_ws = guest_tree.append_empty_workspace("Placeholder");
            assert!(
                guest_tree.set_room(guest_ws, room_a),
                "the guest joins room A"
            );
            // The invite names the host (hub 1) as room A's governor.
            assert!(guest_tree.pin_room_authority(room_a, PeerOrigin::hub(1)));

            let tree_a = host_tree.shared();
            let tree_b = guest_tree.shared();
            tree_a.write().sync_room_doc(room_a);

            let before = tree_b.read().desktop.room_epoch();

            let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree_a.clone())));
            let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, tree_b.clone())));
            let mut hub = Hub::new();
            hub.attach(Box::new(ArcReplica(rep_a.clone())));
            hub.attach(Box::new(ArcReplica(rep_b.clone())));
            sync_round(&mut hub, &rep_b, &[&rep_a]);

            // Precondition: the round actually delivered something, else the epoch assertion below could pass vacuously.
            assert_eq!(
                tree_a.write().room_heads(room_a),
                tree_b.write().room_heads(room_a),
                "the round must have delivered the room document"
            );
            assert!(
                tree_b.read().desktop.room_epoch() > before,
                "content landed in the guest's tree but nothing told the frame \
                 loop to rebuild, so it would never be drawn"
            );
        }
        // Phase `a_round_that_lands_nothing_is_not_announced`.
        // A round that delivered nothing must not claim otherwise — an epoch
        // ticking on every quiet sync would repaint the whole shell at gossip rate and make the signal useless.
        {
            let (host_tree, a, _b) = host_with_two_rooms();
            let room_a = host_tree.room_of(a).expect("room A");
            let mut guest_tree = WorkspaceTree::default();
            let guest_ws = guest_tree.append_empty_workspace("Placeholder");
            assert!(guest_tree.set_room(guest_ws, room_a));
            assert!(guest_tree.pin_room_authority(room_a, PeerOrigin::hub(1)));

            let tree_a = host_tree.shared();
            let tree_b = guest_tree.shared();
            tree_a.write().sync_room_doc(room_a);

            let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree_a)));
            let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(2, tree_b.clone())));
            let mut hub = Hub::new();
            hub.attach(Box::new(ArcReplica(rep_a.clone())));
            hub.attach(Box::new(ArcReplica(rep_b.clone())));
            sync_round(&mut hub, &rep_b, &[&rep_a]);
            let settled = tree_b.read().desktop.room_epoch();

            // Nothing changed; sync again.
            sync_round(&mut hub, &rep_b, &[&rep_a]);
            assert_eq!(
                tree_b.read().desktop.room_epoch(),
                settled,
                "a sync round that carried no change must not announce one"
            );
        }
    }

    #[test]
    fn a_delayed_reply_preserves_a_local_edit_not_yet_exported() {
        let (tree, workspace, _) = host_with_two_rooms();
        let room = tree.room_of(workspace).expect("room A");
        let tree = tree.shared();
        let old_changes = tree
            .write()
            .export_room_changes_since(room, &[])
            .expect("room snapshot");
        {
            let mut tree = tree.write();
            tree.desktop
                .workspaces
                .iter_mut()
                .find(|w| w.id == workspace)
                .expect("workspace")
                .name = "Renamed locally".into();
            tree.seed_doc_from_cache();
        }
        let mut replica = WorkspaceTreeReplica::new(1, tree.clone());
        replica.handle_from(
            &PeerOrigin::hub(2),
            &NeighbourMsg::SyncResp {
                changes: old_changes,
                room: room.to_string(),
            },
        );
        assert_eq!(
            tree.read()
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == workspace)
                .expect("workspace")
                .name,
            "Renamed locally",
            "a delayed peer response must merge the local edit before projecting the room back into the cache"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn sync_requests_follow_room_membership_and_drain_in_order() {
        // Phase `drain_outgoing_returns_fifo_and_clears`.
        {
            let tree = WorkspaceTree::default().shared();
            let rep = WorkspaceTreeReplica::new(42, tree);

            rep.pending_outgoing
                .lock()
                .unwrap()
                .push(NeighbourMsg::SyncReq {
                    since: vec![1],
                    room: String::new(),
                    invite_id: None,
                    nonce: 0,
                });
            rep.pending_outgoing
                .lock()
                .unwrap()
                .push(NeighbourMsg::SyncReq {
                    since: vec![2],
                    room: String::new(),
                    invite_id: None,
                    nonce: 0,
                });

            let drained = rep.drain_outgoing();
            assert_eq!(drained.len(), 2);
            match &drained[0] {
                NeighbourMsg::SyncReq { since, .. } => assert_eq!(since, &vec![1]),
                other => panic!("expected SyncReq, got {other:?}"),
            }
            match &drained[1] {
                NeighbourMsg::SyncReq { since, .. } => assert_eq!(since, &vec![2]),
                other => panic!("expected SyncReq, got {other:?}"),
            }
            assert!(
                rep.drain_outgoing().is_empty(),
                "drain must clear the queue"
            );
        }
        // Phase `request_sync_unwrap_is_safe_for_finite_payloads`.
        // `request_sync`'s `unwrap_or_default()` turns a `serde_json` failure
        // into a "since: []" full-sync request — wasteful, not catastrophic; pins the success path against a regression to panic.
        {
            let mut tree = WorkspaceTree::default();
            let ws = tree.append_empty_workspace("Only room");
            let room = tree.room_of(ws).expect("a fresh workspace has a room");
            let rep = WorkspaceTreeReplica::new(99, tree.shared());

            rep.request_sync();

            let queued = rep.drain_outgoing();
            assert_eq!(queued.len(), 1, "one SyncReq per room this machine is in");
            match &queued[0] {
                NeighbourMsg::SyncReq { since, room: r, .. } => {
                    assert!(
                        !since.is_empty(),
                        "encoded `since` must be a valid JSON array, got empty vec"
                    );
                    assert_eq!(r, &room.to_string(), "the request names the room");
                }
                other => panic!("expected SyncReq, got {other:?}"),
            }
        }
        // Phase `a_machine_in_no_room_requests_nothing`.
        // A machine in no room asks for nothing — a request naming no room is not a request for everything.
        {
            let rep = WorkspaceTreeReplica::new(99, WorkspaceTree::default().shared());
            rep.request_sync();
            assert!(rep.drain_outgoing().is_empty());
        }
        // Phase `asking_a_room_for_its_changes_is_what_joining_it_looks_like`.
        // Taking part in a room's document is what "joined" means: a peer asking
        // about a room this machine is in has joined it, asking about one it
        // doesn't have joins nothing, and an origin with no peer id never arrived over a wire at all.
        {
            let mut tree = WorkspaceTree::default();
            let ws = tree.append_empty_workspace("Design review");
            let room = tree.room_of(ws).expect("a fresh workspace mints a room");
            tree.adopt_ownerless_workspaces(&"quinten".to_string());
            let mut rep = WorkspaceTreeReplica::new(1, tree.shared());

            let guest = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
            let handle = crate::network::behaviour::peer_liveness_handle(&guest);
            let since = serde_json::to_vec(&Vec::<automerge::ChangeHash>::new()).expect("heads");

            assert!(
                rep.participants_in(room).is_empty(),
                "nobody has asked about this room yet",
            );
            let epoch = rep.participants_epoch();
            let arrivals = || epoch.load(std::sync::atomic::Ordering::Acquire);

            // A room this machine is not in — answering nothing is tested elsewhere; this also leaves no trace of a participant.
            let elsewhere = RoomId::new();
            rep.handle_from(
                &PeerOrigin::peer(&guest),
                &NeighbourMsg::SyncReq {
                    since: since.clone(),
                    room: elsewhere.to_string(),
                    invite_id: None,
                    nonce: 0,
                },
            );
            assert!(
                rep.participants_in(elsewhere).is_empty(),
                "a peer asking about a room this machine does not have has \
                 joined nothing here",
            );

            // An in-process subscriber has no peer id, so there's nobody to record — nothing mistakable for someone who turned up on the network.
            rep.handle_from(
                &PeerOrigin::hub(9),
                &NeighbourMsg::SyncReq {
                    since: since.clone(),
                    room: room.to_string(),
                    invite_id: None,
                    nonce: 0,
                },
            );
            assert!(
                rep.participants_in(room).is_empty(),
                "a hub subscriber is not a peer that arrived: {:?}",
                rep.participants_in(room),
            );
            assert_eq!(
                arrivals(),
                0,
                "nobody arrived, so the roster has nothing to re-read"
            );

            let ask = NeighbourMsg::SyncReq {
                since,
                room: room.to_string(),
                invite_id: None,
                nonce: 0,
            };
            rep.handle_from(&PeerOrigin::peer(&guest), &ask);
            assert!(
                rep.participants_in(room).contains(&handle),
                "a peer that asked this room for its changes is in it: {:?}",
                rep.participants_in(room),
            );
            assert_eq!(arrivals(), 1, "an arrival is announced to the roster");
            rep.handle_from(&PeerOrigin::peer(&guest), &ask);
            assert_eq!(
                arrivals(),
                1,
                "asking again is not arriving again, so the roster is not re-read",
            );
        }
    }

    #[test]
    fn replica_tolerates_failures_and_exports_chrome_workspaces() {
        // Phase `apply_changes_failure_is_logged`.
        // Verifies `handle()` returns cleanly (no panic/propagation) on a
        // malformed `SyncResp` — production logs via `tracing::error!` instead.
        {
            let mut tree = WorkspaceTree::default();
            let ws = tree.append_empty_workspace("Mine");
            let room = tree.room_of(ws).expect("room");
            let handle = tree.shared();
            let mut rep = WorkspaceTreeReplica::new(11, handle.clone());

            // Snapshot pre-state to prove the room was *not* mutated by the failed apply.
            let heads_before = handle.write().room_heads(room);

            // Bogus byte vec — `Change::try_from(&[..])` fails before any mutation lands, so `apply_room_changes` returns `Err`.
            let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF];
            rep.handle(
                &PeerOrigin::hub(7),
                &NeighbourMsg::SyncResp {
                    changes: vec![garbage],
                    room: room.to_string(),
                },
            );

            // Didn't panic (we got here); room must not have been mutated — heads stay identical.
            let heads_after = handle.write().room_heads(room);
            assert_eq!(
                heads_before, heads_after,
                "apply failed but the room's heads changed — fix allowed partial \
                 application?"
            );
            // No outbound traffic queued: SyncResp doesn't reply.
            assert!(
                rep.pending_outgoing.lock().unwrap().is_empty(),
                "failed SyncResp must not enqueue outbound traffic"
            );
        }
        // Phase `unhandled_message_variants_are_ignored`.
        {
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
        // Phase `workspace_added_via_chrome_is_visible_in_peer_sync_export`.
        // A workspace appended via the shell chrome's `add_workspace` must be
        // visible to the peer-sync replica's export — they share the same `WorkspaceTreeHandle`.
        {
            use crate::shell::chrome_state::ChromeState;
            use crate::shell::state::WorkspaceTreeHandleExt;

            // Construct via the handle so chrome + replica share the same
            // tree; starts empty so every chrome mutation is captured.
            let handle: WorkspaceTreeHandle = WorkspaceTreeHandle::test_default();
            let replica = WorkspaceTreeReplica::new(42, handle.clone());
            let chrome = ChromeState::new_with_handle(handle.clone());

            // Production path the wheel-disc's "+" slot hits; mints the new workspace's room.
            chrome.borrow_mut().add_workspace("New");
            let room = {
                let tree = handle.read();
                tree.desktop
                    .workspaces
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
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
use tilo_lock::MutexRecover as _;

#[cfg(test)]
mod native_owner_gate_tests {
    use super::*;
    #[test]
    fn native_enrolled_replica_never_exports_or_applies_legacy_governance() {
        let (directory, owner, room, _, _) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let path = directory.path().join("state.json");
        let before = std::fs::read(&path).unwrap();
        let mut loaded = crate::shell::persist::load_tree_from(&path).unwrap();
        loaded.persist_path = Some(path.clone());
        let mut replica = WorkspaceTreeReplica::new(13, loaded.shared());
        replica.handle_from(
            &PeerOrigin::hub(17),
            &NeighbourMsg::SyncReq {
                since: vec![],
                room: room.to_string(),
                invite_id: None,
                nonce: 0,
            },
        );
        assert!(
            replica.drain_outgoing().is_empty(),
            "enrolled room cannot export a synthetic legacy checkpoint"
        );
        replica.handle_from(
            &PeerOrigin::hub(17),
            &NeighbourMsg::SyncResp {
                changes: vec![vec![1, 2, 3]],
                room: room.to_string(),
            },
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(owner.with_current(room, |_| Ok(())).is_ok());
    }
}
