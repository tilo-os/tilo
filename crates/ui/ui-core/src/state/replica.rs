//! An app's state: the document it holds of its record and, with Tilo's model, the undo
//! history it keeps and what it published of it.

#[cfg(feature = "model")]
use automerge::ChangeHash;
use automerge::{AutoCommit, ObjId};
#[cfg(feature = "model")]
use collab_types::MemberId;
#[cfg(feature = "model")]
use collab_types::history::{UndoEntry, UndoLog};
#[cfg(feature = "model")]
use collaboration::state::{DocumentFrontier, WritingActors, changes_beyond};
#[cfg(feature = "model")]
use tilo_automerge::ApplyIncoming;

/// An app's state: the CRDT document `App` merges and saves.
pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    #[cfg(feature = "model")]
    outbound: DocumentFrontier,
    #[cfg(feature = "model")]
    undo: UndoLog,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self::from_doc(AutoCommit::new())
    }

    /// Wrap an already-loaded document.
    #[cfg_attr(not(feature = "model"), allow(unused_mut))]
    pub fn from_doc(mut doc: AutoCommit) -> Self {
        Self {
            #[cfg(feature = "model")]
            undo: UndoLog::new(&mut doc),
            #[cfg(feature = "model")]
            outbound: DocumentFrontier::at_document(&mut doc),
            doc,
            root: automerge::ROOT,
        }
    }

    /// Restores an application state from a serialized byte vector.
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        Ok(Self::from_doc(AutoCommit::load(data)?))
    }

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Serialize the changes made since the last `save/save_incremental` call.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }

    /// Replace the live document with another one — a different record's. The undo
    /// history goes with it: an undo entry is a pair of change hashes in *this*
    /// document's graph, and carried across a swap it would name changes the new document
    /// has never seen. False when the history's revision counter is exhausted.
    pub fn adopt_doc(&mut self, doc: AutoCommit) -> bool {
        #[cfg(feature = "model")]
        let Some(next) = self.undo.revision().checked_add(1) else {
            return false;
        };
        *self = Self::from_doc(doc);
        #[cfg(feature = "model")]
        self.undo.set_revision(next);
        true
    }
}

#[cfg(feature = "model")]
impl State {
    /// A counter that changes whenever this document's history does.
    pub fn history_revision(&self) -> u64 {
        self.undo.revision()
    }

    /// Commit the current transaction and push a change-based undo entry recording the
    /// heads that bracket this action.
    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    /// Commit the current transaction and push a change-based undo entry, attributing it
    /// to `author` so multi-user undo can isolate users.
    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<MemberId>,
    ) -> bool {
        self.undo
            .commit_action_authored(&mut self.doc, focused_node_idx, metadata, author)
    }

    pub fn commit_action_authored_with_context(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<MemberId>,
        context: Option<tilo_actions::ActionContext>,
    ) -> bool {
        self.undo.commit_action_authored_with_message(
            &mut self.doc,
            focused_node_idx,
            metadata,
            author,
            context.and_then(|context| context.change_message()),
        )
    }

    /// The newest step of this session still in effect, without taking it back.
    pub fn peek_last_undo(&self) -> Option<UndoEntry> {
        self.undo.peek_last_undo(&self.doc)
    }

    /// How many steps of this session are in effect.
    pub fn undo_depth(&self) -> usize {
        self.undo.undo_depth(&self.doc)
    }

    /// Whether `author` has anything to undo / redo.
    pub fn can_undo_as(&self, author: Option<&MemberId>) -> bool {
        self.undo.can_undo_as(&self.doc, author)
    }

    pub fn can_redo_as(&self, author: Option<&MemberId>) -> bool {
        self.undo.can_redo_as(&self.doc, author)
    }

    /// The history this document keeps.
    #[must_use]
    pub fn undo_log(&self) -> &UndoLog {
        &self.undo
    }

    /// The document, its history and its outbound frontier, each borrowed on its own: what a
    /// caller that writes the document and notes the write in its history at once takes.
    pub fn parts_mut(&mut self) -> (&mut AutoCommit, &mut UndoLog, &mut DocumentFrontier) {
        (&mut self.doc, &mut self.undo, &mut self.outbound)
    }

    /// Record an action that has *already* been committed into `doc` out-of-band (e.g. a
    /// `tilo_actions` action the Ctrl+K palette dispatched, which owns its own actor +
    /// timestamp commit boundary). Brackets the undo entry with the caller-supplied
    /// `before` heads, clears the redo stack, and realigns the undo baseline.
    pub fn record_committed_action(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) -> bool {
        self.undo
            .record_committed_action(&mut self.doc, before, focused_node_idx, metadata)
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a change that
    /// reached this document from outside the window.
    pub fn record_committed_action_authored(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<MemberId>,
    ) -> bool {
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            focused_node_idx,
            metadata,
            author,
        )
    }

    /// Rewrite the document to the content it had at `target_heads`, as a NEW forward
    /// change — "restore to this version" from the history scrubber. Takes the same route
    /// as undo: derive the patches from current to target, then write them, so history is
    /// appended to, never rewritten.
    pub fn restore_to_heads(
        &mut self,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<MemberId>,
    ) -> bool {
        self.undo.restore_to_heads(
            &mut self.doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
        )
    }

    /// Undo the most recent action by writing its inverse as a new forward change.
    pub fn undo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo_as(None, current_focused_idx, current_metadata)
    }

    /// Undo on behalf of `author`: the entry point every interactive Ctrl+Z takes. With
    /// an author, undo skips past other people's actions, so one member never
    /// reverts another's work; `None` — an app running with no identity at all — keeps
    /// the plain LIFO behaviour.
    pub fn undo_as(
        &mut self,
        author: Option<&MemberId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo_as(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Redo on behalf of `author` — the mirror of [`Self::undo_as`], so a redo cannot
    /// re-apply an action the local user never undid.
    pub fn redo_as(
        &mut self,
        author: Option<&MemberId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .redo_as(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Redo the most recently undone action by inverting the inverse change (re-applying
    /// the original action's delta as a new forward change).
    pub fn redo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.redo_as(None, current_focused_idx, current_metadata)
    }

    /// Apply an incremental CRDT patch (bytes produced by `AutoCommit::save_incremental`
    /// on another peer) into this document. This is how an incremental
    /// `CollabEvent::Document` is merged on the receiver side — without it, incremental
    /// deltas broadcast by one peer would never reach another peer's CRDT. Returns an
    /// error if `data` is not a valid incremental Automerge patch (corrupt bytes, or
    /// bytes that don't apply to this document's history).
    pub fn load_incremental(&mut self, data: &[u8]) -> Result<usize, automerge::AutomergeError> {
        let before = self.doc.get_heads();
        let n = self.doc.load_incoming_whole(data)?;
        // Keep the undo baseline aligned with the new, merged document state so the
        // *next* local action brackets against a complete picture.
        self.undo.rebase(&mut self.doc);
        self.outbound.observe_changes_after(&mut self.doc, &before);
        // What the payload carried beyond what applied waits for its history here, but it
        // was delivered now and is never sent back.
        self.outbound.observe(changes_beyond(&mut self.doc, data));
        Ok(n)
    }

    /// Merge a complete peer snapshot without replacing local work, then atomically align
    /// undo and outbound frontiers with the merged graph. Returns an error when `data` is
    /// not a valid Automerge snapshot or cannot be merged into this document.
    pub fn merge_snapshot(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        let load = tilo_resource::profiling::Phase::start("committed.load");
        let mut incoming = AutoCommit::load(data)?;
        load.end();
        let before = self.doc.get_heads();
        self.doc.merge_incoming(&mut incoming)?;
        self.undo.rebase(&mut self.doc);
        self.outbound.observe_changes_after(&mut self.doc, &before);
        Ok(())
    }

    /// Prepare primary-route bytes without advancing its frontier. The caller advances
    /// only after its durable queue accepts the payload.
    pub fn pending_outbound_delta(&mut self) -> (Vec<u8>, Vec<ChangeHash>) {
        self.outbound
            .pending_delta(&mut self.doc, &WritingActors::default())
    }

    pub fn advance_outbound(&mut self, heads: Vec<ChangeHash>) {
        self.outbound.advance(heads);
    }
}
