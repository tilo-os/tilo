//! One record's replication beside a peer: what to send it, and what it sent.
//!
//! A host that replicates an open record itself — a page's window over its
//! peer, an attached view over its caller — keeps the record's frontier and
//! sends the record whole the first time, its changes after. Merging a peer's
//! change advances the frontier over exactly the changes it carried, so a
//! route that publishes this replica's own changes never sends it back, and
//! what this replica made stays to go out.

use automerge::ChangeHash;
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;

use super::{DocumentCheckout, DocumentId, ResourceStore};
use crate::state::DocumentFrontier;
use automerge::AutoCommit;

/// A record's replication beside one peer.
#[derive(Default)]
pub struct DocumentSync {
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

impl DocumentSync {
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
        store: &ResourceStore,
        id: &DocumentId,
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
    /// its own changes alone ([`ResourceStore::own_delta`]).
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn take_own(
        &mut self,
        store: &ResourceStore,
        id: &DocumentId,
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
    /// What the mutation carried is noted on the frontier, as this replica's
    /// peers hold it already.
    ///
    /// # Errors
    /// The bytes are not a valid mutation of this record, or it is not open.
    pub fn merge(
        &mut self,
        store: &ResourceStore,
        checkout: &DocumentCheckout,
        mutation: DocumentMutation,
    ) -> Result<bool, String> {
        let (changed, observed) = store.merge_replica(checkout, mutation)?;
        self.frontier.observe(observed);
        Ok(changed)
    }

    /// Merge `mutation`, which the peer delivered as the record at
    /// `heads`; whether it changed. The peer holds the record there, so what
    /// this replica sends it next is only what lies past those heads, never
    /// the record whole: a window that opened a record before its workspace
    /// delivered it would otherwise send a large record back whole, as its
    /// own write.
    ///
    /// # Errors
    /// The bytes are not a valid mutation of this record, or it is not open.
    pub fn merge_delivered(
        &mut self,
        store: &ResourceStore,
        checkout: &DocumentCheckout,
        mutation: DocumentMutation,
        heads: Vec<ChangeHash>,
    ) -> Result<bool, String> {
        if heads.is_empty() {
            return self.merge(store, checkout, mutation);
        }
        let (changed, _) = store.merge_at(checkout, mutation, &heads)?;
        // Every change the mutation carried lies under the heads it stands at.
        self.frontier.hold(heads);
        self.snapshotted = true;
        Ok(changed)
    }

    /// Move this replica's own changes past the frontier and keep them for
    /// [`Self::take_set_aside`]: for a caller that publishes on its own
    /// schedule and cannot take the frontier's heads itself.
    ///
    /// # Errors
    /// The record is not open here or its owner is busy.
    pub fn set_aside(&mut self, store: &ResourceStore, id: &DocumentId) -> Result<(), String> {
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

    /// Keep `mutation`, the last this replica took to send
    /// ([`Self::take_set_aside`], or the record whole the first time), to go
    /// out again: its caller could not send it. The record whole is sent
    /// whole again, as if never sent.
    pub fn put_back(&mut self, mutation: DocumentMutation) {
        match mutation {
            DocumentMutation::Snapshot(_) => *self = Self::new(),
            DocumentMutation::Incremental(bytes) => self.unsent.insert(0, bytes),
        }
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
            .all(|hash| doc.has_change(hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{ResourceOwners, ResourceType};
    use automerge::transaction::Transactable as _;
    use durable_store::MemoryStore;
    use std::rc::Rc;

    fn put_text(doc: &mut AutoCommit, text: &str) {
        let _ = doc.put(automerge::ROOT, "text", text);
    }

    /// An open record holding `text`.
    fn opened(text: &str) -> (ResourceStore, DocumentCheckout) {
        let mut owners = ResourceOwners::with_store(Rc::new(MemoryStore::queued()));
        let store = owners.open("so.tilo.test", "workspace-a").unwrap();
        let want = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        store
            .reserve_resource(&want)
            .unwrap()
            .initialize(|doc| {
                put_text(doc, text);
                Ok(())
            })
            .unwrap();
        (store, want)
    }

    /// Another device's change to the record, built on what it already holds.
    fn peer_change(store: &ResourceStore, want: &DocumentCheckout, text: &str) -> DocumentMutation {
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

    tilo_test_env::scenario! {
        fn a_record_publishes_itself_whole_once_and_then_its_own_changes;

        phase the_first_publication_is_the_record_whole_and_the_rest_are_its_changes;
        {
            let (store, want) = opened("first");
            let mut sync = DocumentSync::new();
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

        phase a_delivered_record_is_not_sent_whole_again;
        {
            let (store, want) = opened("what the workspace delivered");
            let heads = store.replication_snapshot(&want.id).unwrap().1;
            let mut sync = DocumentSync::delivered(heads);
            assert!(sync.snapshotted());
            assert!(sync.take_outbound(&store, &want.id).unwrap().is_none());
        }

        phase a_record_the_peer_delivered_into_an_open_window_is_not_sent_back_whole;
        {
            // The window opened the record before the workspace delivered it.
            let (store, want) = opened("what the window wrote first");
            let delivered = peer_change(&store, &want, "what the workspace holds");
            let DocumentMutation::Incremental(changes) = &delivered else {
                unreachable!()
            };
            let mut whole = store.with(&want.id, |doc| doc.clone()).unwrap();
            whole.load_incremental(changes).unwrap();
            let heads = whole.get_heads();
            let mut sync = DocumentSync::new();
            assert!(
                sync.merge_delivered(&store, &want, DocumentMutation::Snapshot(whole.save()), heads)
                    .unwrap()
            );
            assert!(sync.snapshotted(), "the peer holds the record it delivered");
            assert!(
                sync.take_outbound(&store, &want.id).unwrap().is_none(),
                "nothing it delivered goes back"
            );
            store.with(&want.id, |doc| {
                put_text(doc, "mine");
                doc.commit();
            });
            let next = sync
                .take_outbound(&store, &want.id)
                .unwrap()
                .expect("the window's change");
            let DocumentMutation::Incremental(bytes) = &next.mutation else {
                panic!("only what lies past the delivered heads goes");
            };
            assert_eq!(automerge::Automerge::parse_changes(bytes).unwrap().len(), 1);
        }

        phase a_peers_changes_are_not_this_replicas_to_publish_and_its_own_still_go;
        {
            let (store, want) = opened("ours");
            let mut sync = DocumentSync::new();
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

        phase what_a_caller_could_not_send_is_put_back_to_go_again_first;
        {
            let (store, want) = opened("ours");
            let mut sync = DocumentSync::new();
            let first = sync
                .take_outbound(&store, &want.id)
                .unwrap()
                .expect("a snapshot");
            sync.advance(first.heads);
            sync.put_back(first.mutation);
            assert!(!sync.snapshotted(), "the record whole goes whole again");
            let again = sync
                .take_outbound(&store, &want.id)
                .unwrap()
                .expect("the snapshot again");
            assert!(matches!(again.mutation, DocumentMutation::Snapshot(_)));
            sync.advance(again.heads);

            let mine = |text: &str| {
                store.with(&want.id, |doc| {
                    put_text(doc, text);
                    doc.commit();
                });
            };
            mine("mine");
            sync.set_aside(&store, &want.id).unwrap();
            let Some(DocumentMutation::Incremental(earlier)) = sync.take_set_aside() else {
                panic!("the change was set aside");
            };
            mine("later");
            sync.set_aside(&store, &want.id).unwrap();
            sync.put_back(DocumentMutation::Incremental(earlier.clone()));
            let Some(DocumentMutation::Incremental(all)) = sync.take_set_aside() else {
                panic!("both changes go");
            };
            assert!(all.len() > earlier.len());
            assert_eq!(
                &all[..earlier.len()],
                &earlier[..],
                "what was put back goes first"
            );
        }

        phase own_changes_are_set_aside_until_taken_and_a_peers_never_are;
        {
            let (store, want) = opened("ours");
            let mut sync = DocumentSync::new();
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

        phase changes_that_build_on_nothing_here_are_left_alone;
        {
            let (store, want) = opened("shared");
            let delta = match peer_change(&store, &want, "theirs") {
                DocumentMutation::Incremental(bytes) => bytes,
                DocumentMutation::Snapshot(_) => panic!("a device sends it changes"),
            };
            assert!(
                store
                    .with(&want.id, |doc| DocumentSync::builds_on(doc, &delta))
                    .unwrap()
            );
            let mut empty = AutoCommit::new();
            assert!(!DocumentSync::builds_on(&mut empty, &delta));
        }

        phase a_large_delivered_resource_is_taken_in_without_rebuilding_its_history;
        {
            // A newcomer's window opened the tracker before its workspace delivered it: the
            // delivery is thousands of changes the window lacks, all at once.
            const ISSUES: usize = 2_000;
            let tracker = |want: &DocumentCheckout, store: &ResourceStore| {
                let mut peer = store.with(&want.id, |doc| doc.clone().fork()).unwrap();
                let issues = peer
                    .put_object(automerge::ROOT, "issues", automerge::ObjType::Map)
                    .unwrap();
                peer.commit();
                for at in 0..ISSUES {
                    let issue = peer
                        .put_object(&issues, format!("issue-{at}"), automerge::ObjType::Map)
                        .unwrap();
                    let title = peer
                        .put_object(&issue, "title", automerge::ObjType::Text)
                        .unwrap();
                    peer.splice_text(&title, 0, 0, &format!("Issue {at}"))
                        .unwrap();
                    peer.put(&issue, "status", "open").unwrap();
                    peer.commit();
                }
                peer
            };
            let rebuilt = || automerge::work().rebuilt_changes;

            let (store, want) = opened("what the window wrote first");
            let mut peer = tracker(&want, &store);
            let mut whole = store.with(&want.id, |doc| doc.clone()).unwrap();
            whole.merge(&mut peer).unwrap();
            let heads = peer.get_heads();
            let mut sync = DocumentSync::new();
            let before = rebuilt();
            assert!(
                sync.merge_delivered(&store, &want, DocumentMutation::Snapshot(peer.save()), heads)
                    .unwrap()
            );
            assert_eq!(
                rebuilt() - before,
                0,
                "taking in a delivered resource rebuilds none of the changes it brought"
            );
            let (mut merged, value) = store
                .with(&want.id, |doc| {
                    (doc.get_heads(), doc.committed_document().hydrate(None))
                })
                .unwrap();
            let mut expected = whole.get_heads();
            merged.sort_unstable();
            expected.sort_unstable();
            assert_eq!(merged, expected, "the window holds every change of both");
            assert_eq!(
                value,
                whole.committed_document().hydrate(None),
                "the window reads as the two merged whole"
            );
            assert!(sync.take_outbound(&store, &want.id).unwrap().is_none());

            // A peer's resource merged without its heads is noted change by change: none of
            // them goes back, and none is rebuilt to tell them.
            let (store, want) = opened("ours");
            let mut sync = DocumentSync::new();
            let first = sync.take_outbound(&store, &want.id).unwrap().unwrap();
            sync.advance(first.heads);
            let mut peer = tracker(&want, &store);
            let before = rebuilt();
            assert!(
                sync.merge(&store, &want, DocumentMutation::Snapshot(peer.save()))
                    .unwrap()
            );
            assert_eq!(rebuilt() - before, 0);
            assert!(
                sync.take_own(&store, &want.id).unwrap().is_none(),
                "every change the peer sent is noted as the peer's"
            );
        }

        phase large_changes_a_reader_here_brought_its_copy_to_are_taken_in_as_that_copy;
        {
            // A tracker's import writes hundreds of issues as one change: a page's replica opens
            // it, and hands the window only what changed since the window's heads.
            let write = |peer: &mut AutoCommit, from: usize| {
                let issues = peer
                    .put_object(automerge::ROOT, format!("issues-{from}"), automerge::ObjType::Map)
                    .unwrap();
                for at in from..from + 200 {
                    let issue = peer
                        .put_object(&issues, format!("issue-{at}"), automerge::ObjType::Map)
                        .unwrap();
                    let body = peer
                        .put_object(&issue, "body", automerge::ObjType::Text)
                        .unwrap();
                    peer.splice_text(&body, 0, 0, &"What the issue says. ".repeat(20))
                        .unwrap();
                }
                peer.commit();
            };
            let verified = || automerge::work().verified_ops;
            let delivery = |store: &ResourceStore, want: &DocumentCheckout, from: usize| {
                let (base, mut replica) = store
                    .with(&want.id, |doc| (doc.get_heads(), doc.clone().fork()))
                    .unwrap();
                write(&mut replica, from);
                (replica.save_after(&base), replica)
            };
            let reads_as = |store: &ResourceStore, want: &DocumentCheckout, whole: &mut AutoCommit| {
                let (mut heads, value) = store
                    .with(&want.id, |doc| {
                        (doc.get_heads(), doc.committed_document().hydrate(None))
                    })
                    .unwrap();
                let mut expected = whole.get_heads();
                heads.sort_unstable();
                expected.sort_unstable();
                assert_eq!(heads, expected, "the window holds every change");
                assert_eq!(value, whole.committed_document().hydrate(None));
            };

            // The replica on this thread holds the resource at the heads it delivers: the window
            // takes a copy, and parses or applies none of the changes again.
            tilo_automerge::shared::clear();
            let (store, want) = opened("ours");
            let mut sync = DocumentSync::new();
            let (changes, mut replica) = delivery(&store, &want, 0);
            tilo_automerge::shared::hold_at(changes.len(), &replica);
            let before = verified();
            assert!(
                sync.merge_delivered(
                    &store,
                    &want,
                    DocumentMutation::Incremental(changes),
                    replica.get_heads(),
                )
                .unwrap()
            );
            assert_eq!(verified() - before, 0, "none of the changes was parsed");
            reads_as(&store, &want, &mut replica);
            assert!(sync.take_outbound(&store, &want.id).unwrap().is_none());

            // A window with a change of its own past what the replica holds keeps it, and still
            // parses none of the changes.
            let (changes, mut replica) = delivery(&store, &want, 200);
            tilo_automerge::shared::hold_at(changes.len(), &replica);
            store.with(&want.id, |doc| {
                put_text(doc, "mine");
                doc.commit();
            });
            let mut whole = store.with(&want.id, |doc| doc.clone()).unwrap();
            whole.merge(&mut replica).unwrap();
            let before = verified();
            assert!(
                sync.merge_delivered(
                    &store,
                    &want,
                    DocumentMutation::Incremental(changes),
                    replica.get_heads(),
                )
                .unwrap()
            );
            assert_eq!(verified() - before, 0, "none of the changes was parsed");
            reads_as(&store, &want, &mut whole);

            // Held by nobody here, the changes are parsed once and applied.
            tilo_automerge::shared::clear();
            let (changes, mut replica) = delivery(&store, &want, 400);
            let ops = automerge::Automerge::parse_changes(&changes)
                .unwrap()
                .iter()
                .map(automerge::Change::len)
                .sum::<usize>();
            let before = verified();
            assert!(
                sync.merge_delivered(
                    &store,
                    &want,
                    DocumentMutation::Incremental(changes),
                    replica.get_heads(),
                )
                .unwrap()
            );
            assert_eq!(
                verified() - before,
                u64::try_from(ops).unwrap(),
                "the changes were parsed once"
            );
            reads_as(&store, &want, &mut replica);
        }
    }
}
