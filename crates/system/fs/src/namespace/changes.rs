//! The namespace document, announcing its own changes.
//!
//! Whatever keeps derived state in step with the namespace (the persister
//! that writes it to disk, the pinned-hydration worker) needs to learn that
//! it changed. Asking on a timer costs a wakeup per tick for the life of the
//! mount, whether anything happened or not. Announcing at each of
//! the dozens of mutation sites would be fragile: a site that forgets loses
//! a change from disk without a sound. So the document itself announces.
//!
//! Changing an Automerge document takes `&mut` access to it, and the only
//! `&mut` path to [`ChangeTrackedDoc`]'s document is its `DerefMut`, which
//! announces. Every edit, today's and tomorrow's, is therefore announced
//! by type. The few `&mut` operations that never move the heads (reading
//! them, saving, generating a sync message, looking up a change) are
//! inherent methods here, which method resolution picks before the deref,
//! so reads and saves stay quiet. Receiving a sync message announces only
//! when it brought changes: an idle exchange carries heads and nothing
//! else. A spurious announcement costs a subscriber one look at the heads;
//! a missed one would leave a change unpersisted, so anything new goes
//! through the deref and announces.
//!
//! Copies share the announcements. Transactional paths (checked restore,
//! the transaction coordinator) edit a clone of the live namespace and then
//! put the clone in its place (`*namespace = candidate`): the subscribers
//! must keep hearing from the namespace that took over, and must hear of the
//! replacement itself. So a clone announces on the same channel, and a
//! document announces when it is dropped, as the replaced one is.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use automerge::sync::SyncDoc;
use automerge::{AutoCommit, ChangeHash};
use tilo_automerge::ApplyIncoming;
use tokio::sync::watch;

/// An [`AutoCommit`] whose every mutable access is announced to
/// [`ChangeTrackedDoc::subscribe`]rs.
pub(super) struct ChangeTrackedDoc {
    doc: AutoCommit,
    /// Moves on with every possible change. The value carries nothing: a
    /// subscriber learns "something may have changed since you last looked"
    /// and looks. Shared with every clone.
    changes: Arc<watch::Sender<()>>,
}

impl ChangeTrackedDoc {
    pub(super) fn new(doc: AutoCommit) -> Self {
        Self {
            doc,
            changes: Arc::new(watch::Sender::new(())),
        }
    }

    /// Tell subscribers the namespace may have changed: for state beside
    /// the document that subscribers also persist (sync progress).
    pub(super) fn announce(&self) {
        self.changes.send_modify(|()| {});
    }

    /// A receiver that sees every announcement made after this call.
    pub(super) fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    // Quiet `&mut` operations: none of these changes what the document
    // holds. (`get_heads` and the saves close a pending transaction, but
    // that transaction's edits were announced when they were made.)

    pub(super) fn get_heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    pub(super) fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub(super) fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }

    pub(super) fn save_after(&mut self, heads: &[ChangeHash]) -> Vec<u8> {
        self.doc.save_after(heads)
    }

    pub(super) fn diff_obj(
        &mut self,
        obj: &automerge::ObjId,
        before: &[ChangeHash],
        after: &[ChangeHash],
        recursive: bool,
    ) -> Result<Vec<automerge::Patch>, automerge::AutomergeError> {
        self.doc.diff_obj(obj, before, after, recursive)
    }

    /// The changes this document holds beyond `heads`.
    pub(super) fn get_changes(&mut self, heads: &[ChangeHash]) -> Vec<automerge::Change> {
        self.doc.get_changes(heads)
    }

    pub(super) fn has_change(&mut self, hash: &ChangeHash) -> bool {
        self.doc.get_change_meta_by_hash(hash).is_some()
    }

    pub(super) fn generate_sync_message(
        &mut self,
        state: &mut automerge::sync::State,
    ) -> Option<automerge::sync::Message> {
        self.doc.sync().generate_sync_message(state)
    }

    /// Applies a sync message, announcing only if it moved the heads.
    /// Returns whether it did.
    pub(super) fn receive_sync_message(
        &mut self,
        state: &mut automerge::sync::State,
        message: automerge::sync::Message,
    ) -> Result<bool, automerge::AutomergeError> {
        let before = self.doc.get_heads();
        let received = self.doc.receive_incoming_sync(state, message);
        let changed = self.doc.get_heads() != before;
        if changed {
            self.announce();
        }
        received.map(|()| changed)
    }
}

impl Clone for ChangeTrackedDoc {
    /// A copy announces to this document's subscribers: it may take this
    /// one's place.
    fn clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            changes: Arc::clone(&self.changes),
        }
    }
}

impl Drop for ChangeTrackedDoc {
    /// A document dropped may have been replaced by a copy holding other
    /// changes; subscribers look again.
    fn drop(&mut self) {
        self.announce();
    }
}

impl Deref for ChangeTrackedDoc {
    type Target = AutoCommit;

    fn deref(&self) -> &AutoCommit {
        &self.doc
    }
}

impl DerefMut for ChangeTrackedDoc {
    fn deref_mut(&mut self) -> &mut AutoCommit {
        self.announce();
        &mut self.doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    #[test]
    fn edits_announce_and_reads_and_saves_do_not() {
        let mut doc = ChangeTrackedDoc::new(AutoCommit::new());
        let mut changes = doc.subscribe();
        let _ = doc.get_heads();
        let _ = doc.save();
        let _ = doc.save_incremental();
        let heads = doc.get_heads();
        let _ = doc.save_after(&heads);
        let _ = doc.generate_sync_message(&mut automerge::sync::State::new());
        assert!(
            !changes.has_changed().unwrap(),
            "a read, a save or a sync message announced a change"
        );

        doc.put(&ROOT, "name", "value").unwrap();
        assert!(changes.has_changed().unwrap(), "an edit was not announced");
        changes.borrow_and_update();

        // A copy edited and put in the original's place, as a transaction
        // commits: its edits and the replacement are announced, and the
        // subscription keeps working afterwards.
        let mut copy = doc.clone();
        copy.put(&ROOT, "other", "value").unwrap();
        assert!(
            changes.has_changed().unwrap(),
            "a copy's edit was not announced"
        );
        changes.borrow_and_update();
        doc = copy;
        assert!(
            changes.has_changed().unwrap(),
            "the replacement was not announced"
        );
        changes.borrow_and_update();
        doc.put(&ROOT, "after", "value").unwrap();
        assert!(
            changes.has_changed().unwrap(),
            "an edit after the replacement was not announced"
        );
    }

    #[test]
    fn a_sync_message_announces_only_what_it_brings() {
        let mut ours = ChangeTrackedDoc::new(AutoCommit::new());
        let mut theirs = AutoCommit::new();
        theirs.put(&ROOT, "name", "value").unwrap();
        let mut changes = ours.subscribe();
        let (mut our_state, mut their_state) =
            (automerge::sync::State::new(), automerge::sync::State::new());
        let mut brought = false;
        for _ in 0..8 {
            let ours_out = ours.generate_sync_message(&mut our_state);
            let ours_sent = ours_out.is_some();
            if let Some(message) = ours_out {
                theirs
                    .receive_incoming_sync(&mut their_state, message)
                    .unwrap();
            }
            let theirs_out = theirs.sync().generate_sync_message(&mut their_state);
            let theirs_sent = theirs_out.is_some();
            if let Some(message) = theirs_out {
                brought |= ours.receive_sync_message(&mut our_state, message).unwrap();
            }
            if !ours_sent && !theirs_sent {
                break;
            }
        }
        assert!(brought, "the exchange brought the peer's change");
        assert!(
            changes.has_changed().unwrap(),
            "that change was not announced"
        );
        changes.borrow_and_update();

        // In sync now: a further exchange carries heads only.
        let message = theirs.sync().generate_sync_message(&mut their_state);
        if let Some(message) = message {
            assert!(!ours.receive_sync_message(&mut our_state, message).unwrap());
        }
        assert!(
            !changes.has_changed().unwrap(),
            "an exchange that brought nothing announced a change"
        );
    }
}
