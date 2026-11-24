//! A document as a replica holds it: the frontier each destination is known to have, the
//! writers this replica wrote it as, and an app's [`State`] — its document and the undo
//! history it keeps.

use automerge::{ActorId, AutoCommit, ChangeHash, ObjId};
use collab_types::MemberId;
use std::collections::HashSet;
use tilo_automerge::ApplyIncoming;
use tilo_resource_store::history::{UndoEntry, UndoLog};

/// The actors this replica has written one document as, beyond the ones the document
/// itself names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WritingActors {
    actors: HashSet<ActorId>,
}

impl WritingActors {
    /// Note that this replica writes as `actor`.
    pub fn note(&mut self, actor: ActorId) {
        self.actors.insert(actor);
    }

    /// Whether `actor` is a writer of this replica.
    #[must_use]
    pub fn contains(&self, actor: &ActorId) -> bool {
        self.actors.contains(actor)
    }
}

/// An explicit outbound synchronization frontier for one document route. A frontier
/// instead names the exact heads one destination is known to have, so serialization is
/// independent of unrelated callers and can later be advanced from an acknowledgement
/// rather than merely from a send attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentFrontier {
    heads: Vec<ChangeHash>,
    /// Changes the destination delivered. It holds them, and they are never sent back;
    /// they are kept apart from `heads` because a delivered change can build on one this
    /// device has not sent yet, and that one still has to go out.
    observed: Vec<ChangeHash>,
}

impl DocumentFrontier {
    /// Start at the document's current heads.
    pub fn at_document(doc: &mut AutoCommit) -> Self {
        Self {
            heads: doc.get_heads(),
            observed: Vec::new(),
        }
    }

    /// Serialize everything after this frontier and advance it to the heads represented
    /// by that payload.
    pub fn take_delta(&mut self, doc: &mut AutoCommit, own: &WritingActors) -> Vec<u8> {
        let (delta, current) = self.pending_delta(doc, own);
        self.advance(current);
        delta
    }

    /// Everything the document holds that the destination does not, whoever made it: for
    /// a destination that mirrors the record whole, such as a page replicating an open
    /// record, not for a route, which publishes this replica's own changes alone
    /// ([`Self::pending_delta`]).
    pub fn pending_mirror_delta(&mut self, doc: &mut AutoCommit) -> (Vec<u8>, Vec<ChangeHash>) {
        let current = doc.get_heads();
        if current == self.heads {
            return (Vec::new(), current);
        }
        let delta = doc.save_after(&self.heads);
        if delta.is_empty() {
            self.heads.clone_from(&current);
        }
        (delta, current)
    }

    /// Build a delta without advancing the frontier.
    pub fn pending_delta(
        &mut self,
        doc: &mut AutoCommit,
        own: &WritingActors,
    ) -> (Vec<u8>, Vec<ChangeHash>) {
        let current = doc.get_heads();
        if current == self.heads {
            return (Vec::new(), current);
        }
        let delta = self.delta(doc, own);
        if !delta.is_empty() {
            return (delta, current);
        }
        self.advance(current.clone());
        (Vec::new(), current)
    }

    /// What this device holds that the destination does not, as an incremental payload:
    /// this replica's own changes after the frontier, made under the document's current
    /// actor, a writer `own` names, or a handle on this device whose actor carries the
    /// device's own. Another replica's change is never in it, whether or not this
    /// frontier was told it arrived: a change is published by the replica that made it,
    /// and the identity service signs a change for the action that names it.
    pub fn delta(&self, doc: &mut AutoCommit, own: &WritingActors) -> Vec<u8> {
        let actor = doc.get_actor().clone();
        let device = tilo_resource_store::history::authors::DeviceActors::of(&actor);
        let mut bytes = Vec::new();
        for change in doc.get_changes(&self.heads) {
            let mine = change.actor_id() == &actor
                || own.contains(change.actor_id())
                || device.wrote(&change.actor_id().to_hex_string());
            if !mine || self.observed.contains(&change.hash()) {
                continue;
            }
            bytes.extend_from_slice(change.raw_bytes());
        }
        bytes
    }

    /// Advance to heads whose delta a durable destination accepted.
    pub fn advance(&mut self, heads: Vec<ChangeHash>) {
        self.heads = heads;
        self.observed.clear();
    }

    /// The destination holds the document at `heads` too, beside what it held: what is
    /// sent next is what lies past both.
    pub fn hold(&mut self, heads: impl IntoIterator<Item = ChangeHash>) {
        for head in heads {
            if !self.heads.contains(&head) {
                self.heads.push(head);
            }
        }
    }

    /// Treat the document's current state as already observed on this route.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.heads = doc.get_heads();
        self.observed.clear();
    }

    /// Record changes known to have arrived from this destination without claiming that
    /// it received locally pending changes.
    pub fn observe(&mut self, observed: impl IntoIterator<Item = ChangeHash>) {
        for hash in observed {
            if !self.heads.contains(&hash) && !self.observed.contains(&hash) {
                self.observed.push(hash);
            }
        }
    }

    /// Whether the destination already holds the document at `heads`: nothing arrived
    /// since this device was told what it holds, and every change under those heads was
    /// delivered or accepted then.
    pub fn unchanged(&self, heads: &[ChangeHash]) -> bool {
        self.observed.is_empty()
            && self.heads.len() == heads.len()
            && heads.iter().all(|head| self.heads.contains(head))
    }

    /// Note every change `doc` gained since `before` as one the destination already holds.
    pub fn observe_changes_after(&mut self, doc: &mut AutoCommit, before: &[ChangeHash]) {
        self.observe(
            doc.get_changes(before)
                .into_iter()
                .map(|change| change.hash()),
        );
    }

    /// Record what a delivered incremental carried beyond what applied: a change whose
    /// history this replica lacks waits for it, but this route delivered it now, and it
    /// is never sent back.
    fn observe_undelivered(&mut self, doc: &mut AutoCommit, payload: &[u8]) {
        self.observe(changes_beyond(doc, payload));
    }

    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }
}

/// The changes `payload` carries that `doc` does not hold: loading a change whose history
/// is missing keeps it until that history arrives, and it was delivered now all the same.
pub fn changes_beyond(doc: &mut AutoCommit, payload: &[u8]) -> Vec<ChangeHash> {
    let Ok(changes) = automerge::Automerge::parse_changes(payload) else {
        return Vec::new();
    };
    changes
        .into_iter()
        .map(|change| change.hash())
        .filter(|hash| !doc.has_change(hash))
        .collect()
}

/// An app's state: the CRDT document `App` merges and saves.
pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    outbound: DocumentFrontier,
    undo: UndoLog,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Self {
            doc,
            root,
            outbound,
            undo,
        }
    }

    /// Replace the live document with another one — a different record's. The undo
    /// history goes with it: an [`UndoEntry`] is a pair of change hashes in *this*
    /// document's graph, and carried across a swap it would name changes the new document
    /// has never seen.
    pub fn adopt_doc(&mut self, doc: AutoCommit) -> bool {
        let Some(next) = self.undo.revision().checked_add(1) else {
            return false;
        };
        self.doc = doc;
        self.root = automerge::ROOT;
        self.undo = UndoLog::new(&mut self.doc);
        self.outbound = DocumentFrontier::at_document(&mut self.doc);
        self.undo.set_revision(next);
        true
    }

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

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Wrap an already-loaded document.
    pub fn from_doc(mut doc: AutoCommit) -> Self {
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Self {
            doc,
            root: automerge::ROOT,
            outbound,
            undo,
        }
    }

    /// Restores an application state from a serialized byte vector.
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let undo = UndoLog::new(&mut doc);
        let outbound = DocumentFrontier::at_document(&mut doc);
        Ok(Self {
            doc,
            root,
            outbound,
            undo,
        })
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
        self.outbound.observe_undelivered(&mut self.doc, data);
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

    /// Serialize the changes made since the last `save/save_incremental` call.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }
}
