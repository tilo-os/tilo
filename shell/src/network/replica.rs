//! Glue between the in-process [`Hub`] (Phase 1 Task 1.2) and the
//! [`WorkspaceTree`]'s automerge sync API (Phase 1 Task 1.3 / unit A of
//! #157). A [`WorkspaceTreeReplica`] wraps a tree + a peer id + a list of
//! "outbound" deliveries (tests can drain) and implements
//! [`TopicSubscriber`] so a [`Hub`] can fan messages to it.
//!
//! The protocol behaviour:
//!   - `SyncReq { since }` from any peer → reply with
//!     `SyncResp { changes = tree.export_changes_since(&decoded_since) }`.
//!     The reply lands in `pending_outgoing` for the test (or production
//!     loopback driver) to forward via [`Hub::broadcast`].
//!   - `SyncResp { changes }` → call `tree.apply_changes(&changes)`.
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
use crate::shell::state::WorkspaceTree;

/// Replica-side glue: owns a (shared) [`WorkspaceTree`], a peer id, and a
/// FIFO queue of messages to be broadcast on its behalf.
///
/// The tree is held behind `Arc<Mutex<_>>` so a test (or a future
/// production driver) can attach the replica to a [`Hub`] *and* keep a
/// handle for inspecting convergence. Same shape for `pending_outgoing`
/// — the queue is shared so the broadcast driver can drain it without
/// re-entering the replica via the hub.
///
/// [`Hub`]: crate::network::hub::Hub
pub struct WorkspaceTreeReplica {
    pub id: u64,
    pub tree: Arc<Mutex<WorkspaceTree>>,
    pub pending_outgoing: Arc<Mutex<Vec<NeighbourMsg>>>,
}

impl WorkspaceTreeReplica {
    pub fn new(id: u64, tree: Arc<Mutex<WorkspaceTree>>) -> Self {
        Self {
            id,
            tree,
            pending_outgoing: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Build and stash a `SyncReq` carrying this replica's current heads.
    /// Tests call this to kick off a sync round.
    pub fn request_sync(&self) {
        let heads = self.tree.lock().unwrap().current_heads();
        // D-8: `serde_json::to_vec(&Vec<ChangeHash>)` is structurally
        // infallible — there's no enum variant or non-finite float in
        // `ChangeHash`. We keep `unwrap_or_default()` for defence in
        // depth and log loudly if it ever trips; the fallback
        // `vec![]` is interpreted by the recipient as "send me every
        // change", which is wasteful but not corrupting. This is the
        // intentional silent-error path: the failure mode is
        // bandwidth, not data loss.
        let since = serde_json::to_vec(&heads).unwrap_or_else(|err| {
            tracing::error!(
                ?err,
                "serde_json::to_vec(&heads) failed; falling back to a \
                 full-sync request (D-8). This should be unreachable; \
                 if you see this in production, file an issue."
            );
            Vec::new()
        });
        self.pending_outgoing
            .lock()
            .unwrap()
            .push(NeighbourMsg::SyncReq { since });
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

    fn handle(&mut self, _sender: u64, msg: &NeighbourMsg) {
        match msg {
            NeighbourMsg::SyncReq { since } => {
                let since_heads: Vec<automerge::ChangeHash> =
                    serde_json::from_slice(since).unwrap_or_default();
                let mut tree = self.tree.lock().unwrap();
                let changes = tree.export_changes_since(&since_heads);
                drop(tree);
                self.pending_outgoing
                    .lock()
                    .unwrap()
                    .push(NeighbourMsg::SyncResp { changes });
            }
            NeighbourMsg::SyncResp { changes } => {
                let _ = self.tree.lock().unwrap().apply_changes(changes);
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
    use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};

    /// Proxy that forwards `TopicSubscriber` calls into a shared replica
    /// behind `Arc<Mutex<_>>`. Mirrors the `ArcSub` helper in
    /// `hub.rs`'s tests but specialised for `WorkspaceTreeReplica`.
    struct ArcReplica(Arc<Mutex<WorkspaceTreeReplica>>);

    impl TopicSubscriber for ArcReplica {
        fn id(&self) -> u64 {
            self.0.lock().unwrap().id()
        }
        fn handle(&mut self, sender: u64, msg: &NeighbourMsg) {
            self.0.lock().unwrap().handle(sender, msg);
        }
    }

    #[test]
    fn two_replicas_converge_after_one_round_via_hub() {
        // Tree A starts with a manual doc mutation so it has a change to
        // ship. Unit A of #157 doesn't wire any mutator through to the
        // doc yet, so we synthesise a change directly — same approach as
        // `state::automerge_foundation_tests`.
        let mut initial_a = WorkspaceTree::default();
        initial_a
            .doc
            .put(&ROOT, "demo", ScalarValue::Uint(7))
            .unwrap();
        let tree_a = Arc::new(Mutex::new(initial_a));
        let tree_b = Arc::new(Mutex::new(WorkspaceTree::default()));

        // Sanity check: heads diverge before the round.
        let heads_a_before = tree_a.lock().unwrap().current_heads();
        let heads_b_before = tree_b.lock().unwrap().current_heads();
        assert!(!heads_a_before.is_empty(), "A must have a change to ship");
        assert!(heads_b_before.is_empty(), "B starts empty");
        assert_ne!(heads_a_before, heads_b_before);

        let rep_a = Arc::new(Mutex::new(WorkspaceTreeReplica::new(
            1,
            tree_a.clone(),
        )));
        let rep_b = Arc::new(Mutex::new(WorkspaceTreeReplica::new(
            2,
            tree_b.clone(),
        )));

        // Keep handles to inspect outgoing queues + pump the hub.
        let outgoing_a = rep_a.lock().unwrap().pending_outgoing.clone();
        let outgoing_b = rep_b.lock().unwrap().pending_outgoing.clone();

        let mut hub = Hub::new();
        hub.attach(Box::new(ArcReplica(rep_a.clone())));
        hub.attach(Box::new(ArcReplica(rep_b.clone())));

        // Round 1: B asks A "catch me up" (B has no heads, so its since
        // list is empty — A will reply with every change it has).
        rep_b.lock().unwrap().request_sync();

        // Pump B's outbound queue through the hub. Drain via the shared
        // handle so we don't have to re-acquire the replica lock the
        // hub is about to take.
        let drained: Vec<NeighbourMsg> = std::mem::take(&mut *outgoing_b.lock().unwrap());
        assert_eq!(drained.len(), 1, "request_sync produces exactly one SyncReq");
        for msg in drained {
            hub.broadcast(2, msg);
        }

        // A handled the SyncReq and queued a SyncResp. Pump that back.
        let drained_a: Vec<NeighbourMsg> = std::mem::take(&mut *outgoing_a.lock().unwrap());
        assert_eq!(drained_a.len(), 1, "A replies with exactly one SyncResp");
        match &drained_a[0] {
            NeighbourMsg::SyncResp { changes } => {
                assert!(!changes.is_empty(), "A's response must carry the demo change");
            }
            other => panic!("expected SyncResp, got {:?}", other),
        }
        for msg in drained_a {
            hub.broadcast(1, msg);
        }

        // After one round, B has applied A's changes — heads converge.
        let heads_a_after = tree_a.lock().unwrap().current_heads();
        let heads_b_after = tree_b.lock().unwrap().current_heads();
        assert_eq!(
            heads_a_after, heads_b_after,
            "after one hub round, A and B must have identical heads"
        );

        // And B sees the actual demo value.
        let tree_b_guard = tree_b.lock().unwrap();
        let val = tree_b_guard.doc.get(&ROOT, "demo").unwrap();
        assert!(
            val.is_some(),
            "B must observe the demo key A pushed across the hub"
        );
    }

    #[test]
    fn drain_outgoing_returns_fifo_and_clears() {
        let tree = Arc::new(Mutex::new(WorkspaceTree::default()));
        let rep = WorkspaceTreeReplica::new(42, tree);

        rep.pending_outgoing
            .lock()
            .unwrap()
            .push(NeighbourMsg::SyncReq { since: vec![1] });
        rep.pending_outgoing
            .lock()
            .unwrap()
            .push(NeighbourMsg::SyncReq { since: vec![2] });

        let drained = rep.drain_outgoing();
        assert_eq!(drained.len(), 2);
        match &drained[0] {
            NeighbourMsg::SyncReq { since } => assert_eq!(since, &vec![1]),
            other => panic!("expected SyncReq, got {:?}", other),
        }
        match &drained[1] {
            NeighbourMsg::SyncReq { since } => assert_eq!(since, &vec![2]),
            other => panic!("expected SyncReq, got {:?}", other),
        }
        assert!(rep.drain_outgoing().is_empty(), "drain must clear the queue");
    }

    /// D-8 regression: `request_sync` serialises the local heads via
    /// `serde_json::to_vec`. The current code uses
    /// `unwrap_or_default()` which silently turns a serialise failure
    /// into a "since: []" full-sync request — wasteful but not
    /// catastrophic. This test pins the success path so a refactor
    /// that swaps the error handling can't accidentally regress to a
    /// panic on finite payloads.
    ///
    /// We do not test the failure mode because `Vec<ChangeHash>` has
    /// no realistic way to fail `serde_json::to_vec`; the silent-error
    /// path is documented in the production code instead.
    #[test]
    fn request_sync_unwrap_is_safe_for_finite_payloads() {
        let tree = Arc::new(Mutex::new(WorkspaceTree::default()));
        let rep = WorkspaceTreeReplica::new(99, tree);

        // The success path must not panic on a fresh, empty-heads tree.
        rep.request_sync();

        let queued = rep.drain_outgoing();
        assert_eq!(queued.len(), 1, "request_sync must enqueue one SyncReq");
        match &queued[0] {
            NeighbourMsg::SyncReq { since } => {
                // Empty heads serialises to "[]" (2 bytes), not an empty vec.
                assert!(
                    !since.is_empty(),
                    "encoded `since` must be a valid JSON array, got empty vec"
                );
            }
            other => panic!("expected SyncReq, got {:?}", other),
        }
    }

    #[test]
    fn unhandled_message_variants_are_ignored() {
        let tree = Arc::new(Mutex::new(WorkspaceTree::default()));
        let mut rep = WorkspaceTreeReplica::new(7, tree);

        rep.handle(
            1,
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
}
