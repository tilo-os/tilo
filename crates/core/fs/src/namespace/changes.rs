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

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ChangeHash};
use collab_types::FrameworkChange;
use fs_protocol::FileAction;
use tilo_actions::{ActionContext, Described, Did, GroupOf};
use tokio::sync::watch;

/// What the pending edits of one document of the tree did, and for whom: the change they become
/// names it ([`ChangeTrackedDoc::commit_named`]).
#[derive(Clone, Debug, Default)]
struct Naming {
    /// The document's id, as the change's entry names its resource.
    document: String,
    /// The first actions, up to what an entry holds.
    first: Vec<(Did, String)>,
    /// How many actions the edits are.
    count: u32,
    /// Every distinct verb among them.
    distinct: Vec<Did>,
    /// Whether every action named the first one's node: a file made and written, say, which
    /// that file's History lists.
    one_node: bool,
    /// Who they were done by and in which call: `None` for the person at this device.
    ambient: Option<ActionContext>,
}

impl Naming {
    fn context(&self) -> Option<ActionContext> {
        let ((first, item), rest) = self.first.split_first()?;
        let described = if rest.is_empty() && self.count == 1 {
            Described::new(first.clone())
        } else {
            Described::new(Did::Group(GroupOf {
                members: self.first.iter().map(|(did, _)| did.clone()).collect(),
                count: self.count,
                distinct: self.distinct.clone(),
            }))
        };
        let described = if self.one_node {
            described.on(item.clone(), item.clone())
        } else {
            described
        };
        let context = fs_protocol::file_action::change_context(&self.document, described, None);
        Some(match &self.ambient {
            Some(ambient) => context.enriched_by(ambient),
            None => context,
        })
    }
}

/// An [`AutoCommit`] whose every mutable access is announced to
/// [`ChangeTrackedDoc::subscribe`]rs.
pub(super) struct ChangeTrackedDoc {
    doc: AutoCommit,
    /// What the edits not yet committed did, once one of them was a file action.
    naming: Option<Naming>,
    /// The person whose certified device this mount is, when it is one: who made what the
    /// document committed with no file action naming it (`None` for a bare mount, which is no
    /// person's).
    person: Option<tilo_actions::MemberId>,
    /// Moves on with every possible change. The value carries nothing: a
    /// subscriber learns "something may have changed since you last looked"
    /// and looks. Shared with every clone.
    changes: Arc<watch::Sender<()>>,
}

impl ChangeTrackedDoc {
    pub(super) fn new(doc: AutoCommit, person: Option<tilo_actions::MemberId>) -> Self {
        Self {
            doc,
            naming: None,
            person,
            changes: Arc::new(watch::Sender::new(())),
        }
    }

    /// A document announcing on `with`'s channel: one more record of the same namespace.
    pub(super) fn sharing(doc: AutoCommit, with: &Self) -> Self {
        Self {
            doc,
            naming: None,
            person: with.person.clone(),
            changes: Arc::clone(&with.changes),
        }
    }

    /// Write `action` into this document, the document `document` of the tree, done in `ambient`
    /// (who did it and in which call; `None`: the person at this device). Edits of one writer gather into one change, as
    /// a folder copied in is one change per document; a different writer's edit closes the
    /// change first, so each change names only who made it.
    pub(super) fn write_action(
        &mut self,
        document: &str,
        action: &FileAction,
        ambient: Option<&ActionContext>,
    ) -> Result<(), tilo_actions::ActionError> {
        if self
            .naming
            .as_ref()
            .is_some_and(|naming| naming.ambient.as_ref() != ambient)
            || (self.naming.is_none() && self.doc.pending_ops() > 0)
        {
            self.commit_named();
        }
        action.write(self)?;
        self.name(document, action.described().what, action.node(), ambient);
        Ok(())
    }

    /// Name what this document changed, and no file action wrote, as the tree's verb `verb`
    /// on `node`, done in `ambient`: the tree's own document founded as its root folder's
    /// making, or listing the chunk a new node is made in as that node's making. A member is
    /// granted these as the creating verbs they are part of, never as an edit of the whole
    /// document, which only owners may make of a document with an outbox.
    pub(super) fn name_as(
        &mut self,
        document: &str,
        verb: Did,
        node: &str,
        ambient: Option<&ActionContext>,
    ) {
        if self
            .naming
            .as_ref()
            .is_some_and(|naming| naming.ambient.as_ref() != ambient)
        {
            self.commit_named();
        }
        self.name(document, verb, node, ambient);
    }

    fn name(&mut self, document: &str, verb: Did, node: &str, ambient: Option<&ActionContext>) {
        let naming = self.naming.get_or_insert_with(|| Naming {
            document: document.to_owned(),
            ambient: ambient.cloned(),
            one_node: true,
            ..Naming::default()
        });
        naming.one_node &= naming.first.first().is_none_or(|(_, first)| first == node);
        if naming.first.len() < tilo_actions::entry::MAX_GROUP_MEMBERS {
            naming.first.push((verb.clone(), node.to_owned()));
        }
        naming.count = naming.count.saturating_add(1);
        if !naming.distinct.contains(&verb) {
            naming.distinct.push(verb);
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

    /// Take `doc`'s place as the document, announcing it to the same
    /// subscribers: what a namespace that took a peer's document whole does,
    /// so the state kept beside it follows the document that replaced it.
    pub(super) fn replace_document(&mut self, mut doc: AutoCommit) {
        if doc.get_heads() != self.doc.get_heads() {
            self.doc = doc;
            self.announce();
        }
    }

    // Quiet `&mut` operations: none of these changes what the document
    // holds. (`get_heads` and the saves close a pending transaction, but
    // that transaction's edits were announced when they were made.) Closing
    // it is where the mount's edit becomes a change, so that is where it is
    // named as what it is.

    /// Commit what the document changed as a mount's edit of the tree: the
    /// change names what produced it, which no app action may have, as every change a device
    /// signs must ([`collab_types::check_claimed`]). What no file action named is the mount's
    /// person's own edit of the document; a bare mount is no person's and names nobody.
    pub(super) fn commit_named(&mut self) {
        let naming = self.naming.take();
        if self.doc.pending_ops() == 0 {
            return;
        }
        let message = naming
            .and_then(|naming| naming.context())
            .and_then(|context| context.change_message())
            .unwrap_or_else(|| match &self.person {
                Some(person) => tilo_actions::ActionContext::whole_edit_by(
                    &tilo_actions::Acting::member(person.clone()),
                ),
                None => FrameworkChange::Native.change_message(),
            });
        self.doc
            .commit_with(CommitOptions::default().with_message(message));
    }

    pub(super) fn get_heads(&mut self) -> Vec<ChangeHash> {
        self.commit_named();
        self.doc.get_heads()
    }

    pub(super) fn save(&mut self) -> Vec<u8> {
        self.commit_named();
        self.doc.save()
    }

    pub(super) fn save_incremental(&mut self) -> Vec<u8> {
        self.commit_named();
        self.doc.save_incremental()
    }

    pub(super) fn save_after(&mut self, heads: &[ChangeHash]) -> Vec<u8> {
        self.commit_named();
        self.doc.save_after(heads)
    }

    /// Record the patches of the changes applied from now on: what a peer's
    /// delta touched, and only that, so derived state follows it without a
    /// walk over the document.
    pub(super) fn arm_patches(&mut self) {
        self.commit_named();
        self.doc.update_diff_cursor();
    }

    /// The patches of everything applied since [`Self::arm_patches`].
    /// The document re-arms past them, so [`Self::disarm_patches`] is what
    /// stops the log from growing again.
    pub(super) fn take_patches(&mut self) -> Vec<automerge::Patch> {
        self.commit_named();
        self.doc.diff_incremental()
    }

    pub(super) fn disarm_patches(&mut self) {
        self.commit_named();
        self.doc.reset_diff_cursor();
    }

    /// The changes this document holds beyond `heads`.
    pub(super) fn get_changes(&mut self, heads: &[ChangeHash]) -> Vec<automerge::Change> {
        self.commit_named();
        self.doc.get_changes(heads)
    }

    pub(super) fn has_change(&mut self, hash: &ChangeHash) -> bool {
        self.commit_named();
        self.doc.get_change_meta_by_hash(hash).is_some()
    }
}

impl Clone for ChangeTrackedDoc {
    /// A copy announces to this document's subscribers: it may take this
    /// one's place.
    fn clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            naming: self.naming.clone(),
            person: self.person.clone(),
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
        let mut doc = ChangeTrackedDoc::new(AutoCommit::new(), None);
        let mut changes = doc.subscribe();
        let _ = doc.get_heads();
        let _ = doc.save();
        let _ = doc.save_incremental();
        let heads = doc.get_heads();
        let _ = doc.save_after(&heads);
        assert!(
            !changes.has_changed().unwrap(),
            "a read or a save announced a change"
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
}
