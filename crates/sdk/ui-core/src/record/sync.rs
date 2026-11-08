//! One record's replication beside a peer: what to send it, and what it sent.
//!
//! A host that replicates an open record itself — a page's window over its
//! peer, an attached view over its caller — keeps the record's frontier and
//! sends the record whole the first time, its changes after. Merging a peer's
//! change advances the frontier over exactly the changes it carried, so
//! nothing the peer sent is echoed back and this replica's own changes stay
//! to go out.

use automerge::ChangeHash;
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;

use super::{RecordCheckout, RecordId, RecordStore};
use crate::automerge::AutoCommit;
use crate::state::DocumentFrontier;

/// A record's replication beside one peer.
#[derive(Default)]
pub struct RecordSync {
    /// Everything the peer holds: what lies past it is this replica's alone.
    frontier: DocumentFrontier,
    /// The peer was sent the record whole; changes after it go as deltas.
    snapshotted: bool,
    /// This replica's own changes taken from the frontier but not sent yet.
    unsent: Vec<Vec<u8>>,
}

/// A publication and the heads it advances the frontier to once accepted.
pub struct Outbound {
    pub mutation: DocumentMutation,
    pub heads: Vec<ChangeHash>,
}

impl RecordSync {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A record the peer already delivered as `heads`: what this replica sends
    /// first is what it changed after them, not the record whole again.
    #[must_use]
    pub fn delivered(heads: Vec<ChangeHash>) -> Self {
        let mut sync = Self::new();
        sync.advance(heads);
        sync
    }

    /// Whether the peer holds the record whole.
    #[must_use]
    pub const fn snapshotted(&self) -> bool {
        self.snapshotted
    }

    /// The peer accepted everything up to `heads`.
    pub fn advance(&mut self, heads: Vec<ChangeHash>) {
        self.frontier.advance(heads);
        self.snapshotted = true;
    }

    /// What this replica sends next: the record whole the first time, the
    /// changes the peer lacks after it, whoever made them. Advance with the
    /// returned heads once the peer accepted them.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn take_outbound(
        &mut self,
        store: &RecordStore,
        id: &RecordId,
    ) -> Result<Option<Outbound>, String> {
        if !self.snapshotted {
            let (bytes, heads) = store
                .replication_snapshot(id)
                .map_err(|error| error.to_string())?;
            return Ok(Some(Outbound {
                mutation: DocumentMutation::Snapshot(bytes),
                heads,
            }));
        }
        let (delta, heads) = store
            .replication_delta(id, &mut self.frontier)
            .map_err(|error| error.to_string())?;
        Ok((!delta.is_empty()).then_some(Outbound {
            mutation: DocumentMutation::Incremental(delta),
            heads,
        }))
    }

    /// What this replica made and the peer lacks, for a route that publishes
    /// its own changes alone ([`RecordStore::own_delta`]).
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn take_own(
        &mut self,
        store: &RecordStore,
        id: &RecordId,
    ) -> Result<Option<Outbound>, String> {
        if !self.snapshotted {
            let (bytes, heads) = store
                .replication_snapshot(id)
                .map_err(|error| error.to_string())?;
            return Ok(Some(Outbound {
                mutation: DocumentMutation::Snapshot(bytes),
                heads,
            }));
        }
        let (delta, heads) = store
            .own_delta(id, &mut self.frontier)
            .map_err(|error| error.to_string())?;
        Ok((!delta.is_empty()).then_some(Outbound {
            mutation: DocumentMutation::Incremental(delta),
            heads,
        }))
    }

    /// Merge a peer's `mutation` into the open record; whether it changed.
    /// What the mutation carried is never sent back on this frontier.
    ///
    /// # Errors
    /// The bytes are not a valid mutation of this record, or it is not open.
    pub fn merge(
        &mut self,
        store: &RecordStore,
        checkout: &RecordCheckout,
        mutation: DocumentMutation,
    ) -> Result<bool, String> {
        let (changed, observed) = store.merge_replica(checkout, mutation)?;
        self.frontier.observe(observed);
        Ok(changed)
    }

    /// Move this replica's own changes past the frontier, to send with
    /// [`Self::take_set_aside`] whenever the caller gets to publish them.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn set_aside(&mut self, store: &RecordStore, id: &RecordId) -> Result<(), String> {
        let Some(own) = self.take_own(store, id)? else {
            return Ok(());
        };
        if let DocumentMutation::Incremental(bytes) = own.mutation {
            self.unsent.push(bytes);
        }
        self.advance(own.heads);
        Ok(())
    }

    /// Everything [`Self::set_aside`] moved since the last call, as one run.
    pub fn take_set_aside(&mut self) -> Option<DocumentMutation> {
        let own: Vec<u8> = std::mem::take(&mut self.unsent).concat();
        (!own.is_empty()).then_some(DocumentMutation::Incremental(own))
    }

    /// Whether `bytes` build only on changes `doc` holds: an incremental run
    /// that builds on nothing here belongs to another record, or arrived
    /// ahead of what it builds on.
    #[must_use]
    pub fn builds_on(doc: &mut AutoCommit, bytes: &[u8]) -> bool {
        let mut incoming = AutoCommit::new();
        if incoming.load_incoming(bytes).is_err() {
            return false;
        }
        incoming
            .get_missing_deps(&[])
            .iter()
            .all(|hash| doc.get_change_by_hash(hash).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{RecordKind, RecordOwners};
    use automerge::transaction::Transactable as _;
    use durable_store::MemoryStore;
    use std::rc::Rc;

    fn put_text(doc: &mut AutoCommit, text: &str) {
        let _ = doc.put(automerge::ROOT, "text", text);
    }

    /// An open record holding `text`.
    fn opened(text: &str) -> (RecordStore, RecordCheckout) {
        let mut owners = RecordOwners::with_store(Rc::new(MemoryStore::queued()));
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        let want = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
        store
            .reserve_record(&want)
            .unwrap()
            .initialize(|doc| {
                put_text(doc, text);
                Ok(())
            })
            .unwrap();
        (store, want)
    }

    /// Another device's change to the record, built on what it already holds.
    fn peer_change(store: &RecordStore, want: &RecordCheckout, text: &str) -> DocumentMutation {
        let (heads, mut peer) = store
            .with(&want.id, |doc| {
                let heads = doc.get_heads();
                (heads, doc.clone().fork())
            })
            .unwrap();
        put_text(&mut peer, text);
        peer.commit();
        DocumentMutation::Incremental(peer.save_after(&heads))
    }

    #[test]
    fn the_first_publication_is_the_record_whole_and_the_rest_are_its_changes() {
        let (store, want) = opened("first");
        let mut sync = RecordSync::new();
        let first = sync
            .take_outbound(&store, &want.id)
            .unwrap()
            .expect("a snapshot");
        assert!(matches!(first.mutation, DocumentMutation::Snapshot(_)));
        assert!(!sync.snapshotted(), "the frontier advances once accepted");
        sync.advance(first.heads);
        assert!(sync.take_outbound(&store, &want.id).unwrap().is_none());
        store.with(&want.id, |doc| {
            put_text(doc, "second");
            doc.commit();
        });
        let next = sync
            .take_outbound(&store, &want.id)
            .unwrap()
            .expect("a delta");
        let DocumentMutation::Incremental(bytes) = &next.mutation else {
            panic!("the record goes whole only the first time");
        };
        assert!(!bytes.is_empty());
    }

    #[test]
    fn a_delivered_record_is_not_sent_whole_again() {
        let (store, want) = opened("what the workspace delivered");
        let heads = store.replication_snapshot(&want.id).unwrap().1;
        let mut sync = RecordSync::delivered(heads);
        assert!(sync.snapshotted());
        assert!(sync.take_outbound(&store, &want.id).unwrap().is_none());
    }

    #[test]
    fn a_peers_changes_are_not_this_replicas_to_publish_and_its_own_still_go() {
        let (store, want) = opened("ours");
        let mut sync = RecordSync::new();
        let first = sync
            .take_outbound(&store, &want.id)
            .unwrap()
            .expect("a snapshot");
        sync.advance(first.heads);
        let theirs = peer_change(&store, &want, "theirs");
        assert!(sync.merge(&store, &want, theirs).unwrap());
        assert!(
            sync.take_own(&store, &want.id).unwrap().is_none(),
            "another replica's change is not published as this one's"
        );
        store.with(&want.id, |doc| {
            put_text(doc, "mine");
            doc.commit();
        });
        let own = sync
            .take_own(&store, &want.id)
            .unwrap()
            .expect("the local change");
        assert!(matches!(own.mutation, DocumentMutation::Incremental(_)));
        sync.advance(own.heads);
        assert!(sync.take_own(&store, &want.id).unwrap().is_none());
    }

    #[test]
    fn own_changes_are_set_aside_until_taken_and_a_peers_never_are() {
        let (store, want) = opened("ours");
        let mut sync = RecordSync::new();
        let first = sync
            .take_outbound(&store, &want.id)
            .unwrap()
            .expect("a snapshot");
        sync.advance(first.heads);
        store.with(&want.id, |doc| {
            put_text(doc, "mine");
            doc.commit();
        });
        sync.set_aside(&store, &want.id).unwrap();
        assert!(
            matches!(
                sync.take_set_aside(),
                Some(DocumentMutation::Incremental(bytes)) if !bytes.is_empty()
            ),
            "what this replica made goes as one run"
        );
        assert!(sync.take_set_aside().is_none(), "taken once");
        let theirs = peer_change(&store, &want, "theirs");
        assert!(sync.merge(&store, &want, theirs).unwrap());
        sync.set_aside(&store, &want.id).unwrap();
        assert!(
            sync.take_set_aside().is_none(),
            "another replica's change is not set aside as this one's"
        );
    }

    #[test]
    fn changes_that_build_on_nothing_here_are_left_alone() {
        let (store, want) = opened("shared");
        let delta = match peer_change(&store, &want, "theirs") {
            DocumentMutation::Incremental(bytes) => bytes,
            DocumentMutation::Snapshot(_) => panic!("a device sends it changes"),
        };
        assert!(
            store
                .with(&want.id, |doc| RecordSync::builds_on(doc, &delta))
                .unwrap()
        );
        let mut empty = AutoCommit::new();
        assert!(!RecordSync::builds_on(&mut empty, &delta));
    }
}
