use crate::permissions::UserId;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, PatchAction, ReadDoc, Value};
use std::collections::HashMap;

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

/// One reversible action on the undo/redo stacks.
///
/// **Collab-safe, change-based undo.** Rather than snapshotting the whole
/// document — which on undo would replace the live `AutoCommit` and silently
/// drop any concurrently-merged remote ops — an entry records only the heads
/// that bracket the local action (`before`/`after`); undo recomputes the
/// inverse and writes it as a new forward change, so concurrent remote edits
/// survive.
#[derive(Clone)]
pub struct UndoEntry {
    pub before: Vec<ChangeHash>,
    pub after: Vec<ChangeHash>,
    pub focused_node_idx: Option<usize>,
    pub widget_metadata: Option<String>,
    pub author: Option<UserId>,
}

/// Separated from [`State`] because a history belongs to a *document*, not a
/// process — a stack shared between two documents would revert against the
/// wrong one, as a second tab sharing the first tab's undo stack would.
#[derive(Default)]
pub struct UndoLog {
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// Heads as of the last commit boundary — the `before` for the next action.
    last_heads: Vec<ChangeHash>,
}

impl UndoLog {
    /// An empty history over `doc` as it stands. The current heads become
    /// the baseline the next action brackets against, so content already
    /// in the document is not attributed to an action nobody performed.
    pub fn new(doc: &mut AutoCommit) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_heads: doc.get_heads(),
        }
    }

    /// Re-baseline against the document's current heads, without touching
    /// the stacks. What a merge of remote changes needs: existing entries
    /// keep naming their own changes (still valid addresses in an
    /// append-only graph) while the *next* local action brackets against
    /// the merged picture.
    pub fn rebase(&mut self, doc: &mut AutoCommit) {
        self.last_heads = doc.get_heads();
    }

    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.last()
    }

    /// Commit `doc`'s open transaction and push an entry bracketing it.
    /// Returns `true` iff this actually committed a change (there were pending
    /// ops). `false` means the transaction was empty — the caller can then skip
    /// any persistence / broadcast work, since the document is byte-identical to
    /// before. This is what lets an ephemeral, non-document interaction (a text
    /// selection drag, a focus move) avoid a full `doc.save()` on every event.
    pub fn commit_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        let before = self.last_heads.clone();
        // Stamp the change with wall-clock time. `AutoCommit::commit` leaves
        // automerge's advisory timestamp at zero, which is why a history pane
        // built over a document committed that way can show authorship but not
        // *when* — and why a date-range filter over it matches nothing.
        if qdoc::history::commit_stamped(doc).is_some() {
            let after = doc.get_heads();
            self.undo_stack.push(UndoEntry {
                before,
                after,
                focused_node_idx,
                widget_metadata: metadata,
                author,
            });
            self.redo_stack.clear();
            self.last_heads = doc.get_heads();
            true
        } else {
            // Nothing to undo, but the baseline still moves up: `last_heads` is
            // where the *next* action starts from, and changes reach a document
            // by routes that never pass through here — a peer's ops merged in,
            // the shared schema genesis. A baseline left behind would put those
            // inside the next action's bracket, and one Ctrl+Z would revert both.
            self.last_heads = doc.get_heads();
            false
        }
    }

    /// Record an action already committed into `doc` out-of-band, bracketed
    /// by the caller-supplied `before` heads. See
    /// [`State::record_committed_action`].
    pub fn record_committed_action(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) {
        self.record_committed_action_authored(doc, before, focused_node_idx, metadata, None);
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a
    /// change that reached this document from *outside* the window (an agent,
    /// or a collaborator's own published operation), applied under the
    /// caller's own actor with no local commit boundary. Without recording it
    /// here, the action is unreachable and `last_heads` stays behind it, so
    /// the next local edit's bracket contains someone else's work and one
    /// Ctrl+Z reverts both. The name is what keeps them apart afterwards.
    pub fn record_committed_action_authored(
        &mut self,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) {
        let after = doc.get_heads();
        if after == before {
            // Nothing to undo, but the baseline moves up all the same — same
            // reason as the empty-transaction path in `commit_action_authored`.
            self.last_heads = after;
            return;
        }
        self.undo_stack.push(UndoEntry {
            before,
            after: after.clone(),
            focused_node_idx,
            widget_metadata: metadata,
            author,
        });
        self.redo_stack.clear();
        self.last_heads = after;
    }

    /// Apply the inverse of `[entry.before, entry.after]` as a NEW forward
    /// change merged into `doc`, returning the `[before, after]` heads of
    /// that inverse so the opposite stack can later invert *it* (redo, or
    /// re-undo). Concurrent remote ops merged in since `entry.after` are
    /// untouched — only what the local action itself changed is rewritten.
    fn apply_inverse(
        &mut self,
        doc: &mut AutoCommit,
        entry: &UndoEntry,
    ) -> (Vec<ChangeHash>, Vec<ChangeHash>) {
        let inverse_before = doc.get_heads();
        write_revert_patches(doc, &entry.after, &entry.before);
        let _ = qdoc::history::commit_stamped(doc);
        let inverse_after = doc.get_heads();
        self.last_heads.clone_from(&inverse_after);
        (inverse_before, inverse_after)
    }

    pub fn undo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        let entry = self.undo_stack.pop()?;
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry);
        self.redo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
    }

    /// Whether `author` may revert `entry`: their own work, and anything
    /// **unattributed**.
    ///
    /// The second half is what lets an author-scoped undo reach entries the
    /// toolkit records for an app's own affordances —
    /// [`Widget::take_committed_action`](crate::widget::Widget::take_committed_action)
    /// (toolbar buttons, context-menu items, palette dispatch) has no author
    /// to give, so those entries land with `author: None`.
    ///
    /// Sound rather than merely expedient: an entry only reaches these stacks
    /// via a *local* commit boundary — a peer's ops arrive through
    /// `State::load_incremental`, which re-baselines without pushing anything.
    /// An entry naming a different person still belongs to them and is still
    /// walked past.
    fn claimable_by(entry: &UndoEntry, author: &UserId) -> bool {
        entry.author.as_ref().is_none_or(|a| a == author)
    }

    pub fn undo_for_author(
        &mut self,
        doc: &mut AutoCommit,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        let idx = self
            .undo_stack
            .iter()
            .rposition(|e| Self::claimable_by(e, author))?;
        let entry = self.undo_stack.remove(idx);
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry);
        self.redo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
    }

    pub fn undo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        match author {
            Some(a) => self.undo_for_author(doc, a, current_focused_idx, current_metadata),
            None => self.undo(doc, current_focused_idx, current_metadata),
        }
    }

    pub fn redo(
        &mut self,
        doc: &mut AutoCommit,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        let entry = self.redo_stack.pop()?;
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry);
        self.undo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
    }

    pub fn redo_as(
        &mut self,
        doc: &mut AutoCommit,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        let Some(a) = author else {
            return self.redo(doc, current_focused_idx, current_metadata);
        };
        let idx = self
            .redo_stack
            .iter()
            .rposition(|e| Self::claimable_by(e, a))?;
        let entry = self.redo_stack.remove(idx);
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry);
        self.undo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
    }

    /// See [`State::restore_to_heads`].
    pub fn restore_to_heads(
        &mut self,
        doc: &mut AutoCommit,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        let current = doc.get_heads();
        if current == target_heads {
            return false;
        }
        // `apply_inverse` writes the delta that leads from `after` back to
        // `before`; naming the live state as `after` and the historical state
        // as `before` is what turns "undo one action" into "travel to this
        // version".
        let entry = UndoEntry {
            before: target_heads.to_vec(),
            after: current,
            focused_node_idx,
            widget_metadata: metadata.clone(),
            author: author.clone(),
        };
        let (inv_before, inv_after) = self.apply_inverse(doc, &entry);
        if inv_before == inv_after {
            return false;
        }
        self.undo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx,
            widget_metadata: metadata,
            author,
        });
        self.redo_stack.clear();
        true
    }
}

/// The unified state object that holds our CRDT document.
pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    pub collab: CollaborationState,
    /// This document's history. Swapped out with the document itself in
    /// [`Self::adopt_doc`] — see there for why it cannot travel.
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
        Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo,
        }
    }

    /// Replace the live document with another one — a different record's.
    ///
    /// The undo history goes with it: an [`UndoEntry`] is a pair of change
    /// hashes in *this* document's graph, and carried across a swap it would
    /// name changes the new document has never seen. Each record's history is
    /// its own, so the stacks reset rather than travel.
    ///
    /// `collab` (remote cursors and presence) is kept: presence is about who
    /// else is here, which does not change because the document on screen did.
    pub fn adopt_doc(&mut self, doc: AutoCommit) {
        self.doc = doc;
        self.root = automerge::ROOT;
        self.undo = UndoLog::new(&mut self.doc);
    }

    /// This document's history, for a caller that drives undo itself.
    pub fn history_mut(&mut self) -> &mut UndoLog {
        &mut self.undo
    }

    /// Commit the current transaction and push a change-based undo entry
    /// recording the heads that bracket this action.
    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    /// Commit the current transaction and push a change-based undo entry,
    /// attributing it to `author` so multi-user undo can isolate users. Returns
    /// `true` iff a change was actually committed (see the [`UndoLog`] method).
    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.undo
            .commit_action_authored(&mut self.doc, focused_node_idx, metadata, author)
    }

    /// Inspect the most recently pushed undo entry without popping it.
    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo.peek_last_undo()
    }

    /// Record an action that has *already* been committed into `doc`
    /// out-of-band (e.g. a `qos_actions` action the Ctrl+K palette dispatched,
    /// which owns its own actor + timestamp commit boundary). Brackets the
    /// undo entry with the caller-supplied `before` heads, clears the redo
    /// stack, and realigns the undo baseline.
    pub fn record_committed_action(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
    ) {
        self.undo
            .record_committed_action(&mut self.doc, before, focused_node_idx, metadata);
    }

    /// As [`Self::record_committed_action`], naming who performed it — for a
    /// change that reached this document from outside the window. See
    /// [`UndoLog::record_committed_action_authored`].
    pub fn record_committed_action_authored(
        &mut self,
        before: Vec<ChangeHash>,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) {
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            focused_node_idx,
            metadata,
            author,
        );
    }

    /// Rewrite the document to the content it had at `target_heads`, as a NEW
    /// forward change — "restore to this version" from the history scrubber.
    /// Takes the same route as undo: derive the patches from current to
    /// target, then write them, so history is appended to, never rewritten.
    ///
    /// Consequences: the restore shows up in the timeline as its own entry
    /// (the versions it rolled past are still there); remote edits merged in
    /// while scrubbing survive, since only what actually differs is
    /// rewritten; and the restore is itself undoable.
    ///
    /// Returns `true` when something changed; a restore to the current state
    /// is a no-op.
    pub fn restore_to_heads(
        &mut self,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        self.undo.restore_to_heads(
            &mut self.doc,
            target_heads,
            focused_node_idx,
            metadata,
            author,
        )
    }

    /// Undo the most recent action by writing its inverse as a new forward
    /// change. Callers pass the *current* focus index and widget metadata so
    /// the redo entry carries the caret/focus state at the moment undo fired
    /// — otherwise Ctrl+Shift+Z would rehydrate the text body but leave the
    /// caret at offset 0.
    pub fn undo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo(&mut self.doc, current_focused_idx, current_metadata)
    }

    /// Multi-user aware undo. Pops the topmost entry whose `author` equals
    /// `author`, writing its inverse as a new forward change. Entries from
    /// other users are preserved in the stack, so one collaborator's undo
    /// never reverts — or clobbers — another's committed work.
    ///
    /// `current_focused_idx` / `current_metadata` are captured on the redo
    /// stack so a subsequent redo can restore the caret — same rationale as
    /// [`undo`].
    pub fn undo_for_author(
        &mut self,
        author: &UserId,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo_for_author(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Undo on behalf of `author`.
    ///
    /// This is the entry point every interactive Ctrl+Z takes, and the reason
    /// it exists rather than callers picking between [`Self::undo`] and
    /// [`Self::undo_for_author`] themselves: with an author, undo must skip
    /// past other people's actions, and a call site that forgets is a session
    /// in which one collaborator silently reverts another's work. `None` — an
    /// app running with no identity at all — keeps the plain LIFO behaviour.
    pub fn undo_as(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .undo_as(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Redo on behalf of `author` — the mirror of [`Self::undo_as`], so a
    /// redo cannot re-apply an action the local user never undid.
    pub fn redo_as(
        &mut self,
        author: Option<&UserId>,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .redo_as(&mut self.doc, author, current_focused_idx, current_metadata)
    }

    /// Redo the most recently undone action by inverting the inverse change
    /// (re-applying the original action's delta as a new forward change).
    /// `current_focused_idx` / `current_metadata` are captured on the undo
    /// stack so a subsequent undo can park the caret back where it was
    /// pre-redo.
    pub fn redo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        self.undo
            .redo(&mut self.doc, current_focused_idx, current_metadata)
    }

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Wrap an already-loaded document.
    ///
    /// The entry point for persistence (`ui::persist`): the store hands back
    /// the previous session's `AutoCommit` — history, actor identity and all
    /// — and this adopts it as the live document without a byte round-trip.
    pub fn from_doc(mut doc: AutoCommit) -> Self {
        let undo = UndoLog::new(&mut doc);
        Self {
            doc,
            root: automerge::ROOT,
            collab: CollaborationState::new(),
            undo,
        }
    }

    /// Restores an application state from a serialized byte vector.
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid Automerge document (corrupt
    /// bytes, or bytes produced by an incompatible format).
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let undo = UndoLog::new(&mut doc);
        Ok(Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo,
        })
    }

    /// Apply an incremental CRDT patch (bytes produced by
    /// `AutoCommit::save_incremental` on another peer) into this document.
    /// Returns the number of ops applied on success.
    ///
    /// This is how `CollabEvent::CRDTIncremental` is actually merged on the
    /// receiver side — without it, incremental deltas broadcast by one peer
    /// would never reach another peer's CRDT.
    ///
    /// # Errors
    ///
    /// Returns an error if `data` is not a valid incremental Automerge patch
    /// (corrupt bytes, or bytes that don't apply to this document's history).
    pub fn load_incremental(&mut self, data: &[u8]) -> Result<usize, automerge::AutomergeError> {
        let n = self.doc.load_incremental(data)?;
        // Keep the undo baseline aligned with the new, merged document state
        // so the *next* local action brackets against a complete picture.
        // Existing entries keep their original before/after heads — those
        // remain valid addresses into the append-only change graph after a
        // merge, so undoing an earlier action still reverts exactly that
        // action, leaving the just-merged remote ops intact.
        self.undo.rebase(&mut self.doc);
        Ok(n)
    }

    /// Serialize the changes made since the last `save/save_incremental` call.
    /// The returned bytes can be fed into another peer's `load_incremental`
    /// to replay just the delta — the basic building block of efficient
    /// CRDT sync.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }
}

/// Write the delta that leads from `from_heads` to `to_heads` into `doc`'s open
/// transaction, without committing.
///
/// The one primitive behind both undo and restore-from-history: it re-derives
/// exactly the properties and elements that differ and writes them forward,
/// rather than replacing the document with an old state and dropping every
/// concurrently-merged remote op.
///
/// The caller commits. That is deliberate: a `ui` app signals `needs_commit()`
/// and lets `App::commit_action` do the commit, so a restore performed from
/// inside a widget lands on the undo stack with the right author and timestamp
/// exactly like any other edit. [`State::restore_to_heads`] is the equivalent
/// for a host that owns a whole [`State`].
pub fn write_revert_patches(
    doc: &mut AutoCommit,
    from_heads: &[ChangeHash],
    to_heads: &[ChangeHash],
) {
    // Patches describing how to get from `from_heads` back to `to_heads`.
    let patches = doc.diff(from_heads, to_heads);
    write_patches(doc, patches, to_heads, from_heads);
}

/// Write an already-derived patch set forward into `doc`'s open transaction.
///
/// The half of [`write_revert_patches`] after the diff, split out so a caller
/// that needs to *filter* the patch set can reuse the index translation rather
/// than reimplement it — [`crate::history_source::revert`] drops the patches
/// that would overwrite somebody's later edit and writes the rest.
///
/// The two anchors are the head sets the patch indices are expressed against.
/// `insert_anchor` is the state in which a re-inserted element's boundary is
/// meaningful (the element does not exist after the action being inverted);
/// `index_anchor` is the state in which an element the patch deletes or
/// overwrites still exists. Passing them the wrong way round silently moves
/// edits to neighbouring positions, so both are named rather than positional.
pub fn write_patches(
    doc: &mut AutoCommit,
    patches: Vec<automerge::Patch>,
    insert_anchor: &[ChangeHash],
    index_anchor: &[ChangeHash],
) {
    let entry_after = index_anchor;
    let entry_before = insert_anchor;
    // Recreated objects get a fresh ExId; map the patch's (old) ObjId to it
    // so nested child patches target the new object.
    let mut remap: HashMap<ObjId, ObjId> = HashMap::new();
    for patch in patches {
        let target = remap
            .get(&patch.obj)
            .cloned()
            .unwrap_or_else(|| patch.obj.clone());
        match patch.action {
            PatchAction::PutMap { key, value, .. } => match value.0 {
                Value::Scalar(s) => {
                    let _ = doc.put(&target, key, s.into_owned());
                }
                Value::Object(ot) => {
                    if let Ok(new_id) = doc.put_object(&target, key, ot) {
                        remap.insert(value.1, new_id);
                    }
                }
            },
            PatchAction::PutSeq { index, value, .. } => {
                // The element still exists post-action; anchor at `after`.
                let li = live_index_at(doc, &target, index, entry_after);
                match value.0 {
                    Value::Scalar(s) => {
                        let _ = doc.put(&target, li, s.into_owned());
                    }
                    Value::Object(ot) => {
                        if let Ok(new_id) = doc.put_object(&target, li, ot) {
                            remap.insert(value.1, new_id);
                        }
                    }
                }
            }
            PatchAction::Insert { index, values } => {
                // Re-inserting elements the local action deleted; the
                // boundary is expressed in `before` coordinates.
                let start = live_insert_boundary(doc, &target, index, entry_before);
                for (li, (val, old_id, _)) in (start..).zip(&values) {
                    match val {
                        Value::Scalar(s) => {
                            let _ = doc.insert(&target, li, s.clone().into_owned());
                        }
                        Value::Object(ot) => {
                            if let Ok(new_id) = doc.insert_object(&target, li, *ot) {
                                remap.insert(old_id.clone(), new_id);
                            }
                        }
                    }
                }
            }
            PatchAction::SpliceText { index, value, .. } => {
                // Re-inserting text the local action deleted.
                let li = live_insert_boundary(doc, &target, index, entry_before);
                let _ = doc.splice_text(&target, li, 0, &value.make_string());
            }
            PatchAction::Increment { prop, value } => {
                let _ = doc.increment(&target, prop, value);
            }
            PatchAction::DeleteMap { key } => {
                let _ = doc.delete(&target, key);
            }
            PatchAction::DeleteSeq { index, length } => {
                // Removing elements the local action inserted; they exist
                // post-action, so each position is anchored at `after`.
                for _ in 0..length {
                    let li = live_index_at(doc, &target, index, entry_after);
                    let _ = doc.delete(&target, li);
                }
            }
            // A conflict marker carries no content to revert, and marks
            // (text formatting spans) are not produced by the editor's
            // undo-tracked actions; both are safely ignored.
            PatchAction::Conflict { .. } | PatchAction::Mark { .. } => {}
        }
    }
}

/// Translate a sequence index expressed as-of `anchor_heads` into its position
/// in the live document, so concurrent inserts/deletes shift it correctly. Used
/// when the referenced element still exists.
fn live_index_at(
    doc: &AutoCommit,
    obj: &ObjId,
    index: usize,
    anchor_heads: &[ChangeHash],
) -> usize {
    match doc.get_cursor(obj, index, Some(anchor_heads)) {
        Ok(cursor) => doc.get_cursor_position(obj, &cursor, None).unwrap_or(index),
        Err(_) => index,
    }
}

/// Translate an *insertion boundary* expressed as-of `anchor_heads` into the
/// live document. The element originally at `index` may itself have been
/// deleted by the action we are reverting (which would trip an automerge
/// fast/slow cursor-seek consistency check), so anchor on the surviving element
/// just before the boundary and insert after it.
fn live_insert_boundary(
    doc: &AutoCommit,
    obj: &ObjId,
    index: usize,
    anchor_heads: &[ChangeHash],
) -> usize {
    if index == 0 {
        return 0;
    }
    // `index >= 1` here (checked above), so this never underflows.
    match doc.get_cursor(obj, index.saturating_sub(1), Some(anchor_heads)) {
        Ok(cursor) => doc
            .get_cursor_position(obj, &cursor, None)
            .map_or(index, |p| p.saturating_add(1)),
        Err(_) => index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ReadDoc, ScalarValue, transaction::Transactable};

    /// A single `commit_action` → undo → redo round-trip must preserve the
    /// focus index and widget metadata captured on the stacks — that's
    /// what restores the caret in `EditorApp` after Ctrl+Shift+Z.
    #[test]
    fn redo_entry_carries_current_focus_and_metadata() {
        let mut state = State::new();

        // Seed a mutation so commit_action has something to push.
        state
            .doc
            .put(&automerge::ROOT, "k", ScalarValue::Str("v".into()))
            .unwrap();
        state.commit_action(Some(7), Some("meta1".into()));

        // Simulate a second mutation whose current caret/focus should
        // be the one we expect redo to rehydrate.
        state
            .doc
            .put(&automerge::ROOT, "k", ScalarValue::Str("v2".into()))
            .unwrap();

        // Undo with *current* state (Some(3) / "meta_current") — this is
        // what callers like `App::commit_action` pass through.
        let popped = state
            .undo(Some(3), Some("meta_current".into()))
            .expect("undo must pop the seeded entry");
        assert_eq!(popped.focused_node_idx, Some(7));
        assert_eq!(popped.widget_metadata, Some("meta1".into()));

        // Redo — the test-only callers here don't need to bump the stack,
        // so pass None/None. The returned entry must reflect the state
        // that was current at the moment `undo(...)` fired.
        let redone = state
            .redo(None, None)
            .expect("redo must pop the redo-stack entry");
        assert_eq!(redone.focused_node_idx, Some(3));
        assert_eq!(redone.widget_metadata, Some("meta_current".into()));

        // Sanity: the doc itself is back at the post-second-mutation
        // state so renderers stay in sync.
        let (val, _) = state.doc.get(&automerge::ROOT, "k").unwrap().unwrap();
        assert_eq!(val.to_str(), Some("v2"));
    }

    use crate::permissions::UserId;
    use automerge::{ObjType, ROOT};

    fn text_body(state: &State, obj: &ObjId) -> String {
        state.doc.text(obj).unwrap_or_default()
    }

    /// A committed local action, then a *concurrent remote change* merged in,
    /// then `undo()`. The remote change MUST survive and only the local
    /// action is reverted.
    #[test]
    fn undo_preserves_concurrent_remote_edit() {
        let mut state = State::new();
        let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
        state.commit_action(None, None);

        // Capture the heads a remote peer would have forked from (pre local action).
        let before_local = state.doc.get_heads();

        // LOCAL action: append " World".
        state.doc.splice_text(&text, 5, 0, " World").unwrap();
        state.commit_action(None, None);

        // CONCURRENT remote edit forked from before the local action: prepend "X".
        let mut remote = state.doc.fork_at(&before_local).unwrap();
        let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
        remote.splice_text(&rtext, 0, 0, "X").unwrap();
        remote.commit();
        // Merge the remote delta into the live doc the way collab sync does.
        let delta = remote.save_after(&before_local);
        state.load_incremental(&delta).unwrap();
        assert_eq!(
            text_body(&state, &text),
            "XHello World",
            "precondition merge"
        );

        // UNDO the local action.
        state.undo(None, None).expect("undo pops the local action");

        // Local " World" reverted; remote "X" survives.
        assert_eq!(
            text_body(&state, &text),
            "XHello",
            "remote prepend survives; only local append reverted"
        );
    }

    /// Redoing the undone local action must reinstate it while *still* keeping
    /// the concurrently-merged remote edit.
    #[test]
    fn redo_after_concurrent_merge_keeps_both() {
        let mut state = State::new();
        let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        state.doc.splice_text(&text, 0, 0, "Hello").unwrap();
        state.commit_action(None, None);
        let before_local = state.doc.get_heads();
        state.doc.splice_text(&text, 5, 0, " World").unwrap();
        state.commit_action(None, None);

        let mut remote = state.doc.fork_at(&before_local).unwrap();
        let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
        remote.splice_text(&rtext, 0, 0, "X").unwrap();
        remote.commit();
        let delta = remote.save_after(&before_local);
        state.load_incremental(&delta).unwrap();

        state.undo(None, None).unwrap();
        assert_eq!(text_body(&state, &text), "XHello");
        state.redo(None, None).unwrap();
        assert_eq!(
            text_body(&state, &text),
            "XHello World",
            "redo reinstates local append; remote prepend still present"
        );
    }

    /// Single-user undo/redo round-trip on a map value returns to the exact
    /// pre/post values.
    #[test]
    fn single_user_undo_redo_roundtrip() {
        let mut state = State::new();
        state
            .doc
            .put(&ROOT, "k", ScalarValue::Str("a".into()))
            .unwrap();
        state.commit_action(None, None);
        state
            .doc
            .put(&ROOT, "k", ScalarValue::Str("b".into()))
            .unwrap();
        state.commit_action(None, None);

        let val = |s: &State| {
            s.doc
                .get(&ROOT, "k")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(val(&state), "b");
        state.undo(None, None).unwrap();
        assert_eq!(val(&state), "a", "undo reverts to pre-action value");
        state.redo(None, None).unwrap();
        assert_eq!(val(&state), "b", "redo restores the action");
    }

    /// Multiple actions undo in LIFO order, each reverting only its own change.
    #[test]
    fn multi_action_undo_order_is_lifo() {
        let mut state = State::new();
        state.doc.put(&ROOT, "a", 1).unwrap();
        state.commit_action(None, None);
        state.doc.put(&ROOT, "b", 2).unwrap();
        state.commit_action(None, None);
        state.doc.put(&ROOT, "c", 3).unwrap();
        state.commit_action(None, None);

        let has = |s: &State, k: &str| s.doc.get(&ROOT, k).unwrap().is_some();
        assert!(has(&state, "a") && has(&state, "b") && has(&state, "c"));

        state.undo(None, None).unwrap(); // reverts "c"
        assert!(has(&state, "a") && has(&state, "b") && !has(&state, "c"));
        state.undo(None, None).unwrap(); // reverts "b"
        assert!(has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
        state.undo(None, None).unwrap(); // reverts "a"
        assert!(!has(&state, "a") && !has(&state, "b") && !has(&state, "c"));
    }

    /// `undo_for_author` reverts only the requested author's most-recent action
    /// as a new forward change, leaving other authors' committed ops intact in
    /// the live document — not just unpopped from the stack.
    #[test]
    fn undo_for_author_reverts_only_that_authors_op() {
        let mut state = State::new();
        // Alice sets a=1
        state.doc.put(&ROOT, "a", 1).unwrap();
        state.commit_action_authored(None, None, Some(UserId("alice".into())));
        // Bob sets b=1
        state.doc.put(&ROOT, "b", 1).unwrap();
        state.commit_action_authored(None, None, Some(UserId("bob".into())));

        // Alice undoes — only "a" is reverted; Bob's "b" stays in the doc.
        let popped = state
            .undo_for_author(&UserId("alice".into()), None, None)
            .expect("alice's entry");
        assert_eq!(popped.author, Some(UserId("alice".into())));
        assert!(
            state.doc.get(&ROOT, "a").unwrap().is_none(),
            "alice's change reverted in the live doc"
        );
        assert!(
            state.doc.get(&ROOT, "b").unwrap().is_some(),
            "bob's change survives in the live doc, not just on the stack"
        );
    }

    /// `undo_as` is the entry point `App`'s Ctrl+Z takes. With an author it
    /// must behave like `undo_for_author` — walk past a collaborator's action
    /// to the local user's own — not like plain LIFO undo.
    #[test]
    fn undo_as_an_author_skips_a_collaborators_action() {
        let mut state = State::new();
        let alice = UserId("alice".into());
        let bob = UserId("bob".into());

        state.doc.put(&ROOT, "a", 1).unwrap();
        state.commit_action_authored(None, None, Some(alice.clone()));
        state.doc.put(&ROOT, "b", 1).unwrap();
        // `bob` is never referenced again, so this is its last use.
        state.commit_action_authored(None, None, Some(bob));

        // Alice hits Ctrl+Z. The newest entry is Bob's.
        let popped = state
            .undo_as(Some(&alice), None, None)
            .expect("alice has an action to undo");
        assert_eq!(
            popped.author,
            Some(alice.clone()),
            "alice undid her own action"
        );
        assert!(
            state.doc.get(&ROOT, "a").unwrap().is_none(),
            "alice's own change is reverted"
        );
        assert!(
            state.doc.get(&ROOT, "b").unwrap().is_some(),
            "bob's change survives alice's undo"
        );
    }

    /// With no identity at all — an app running outside any session — `undo_as`
    /// degrades to plain LIFO rather than refusing to undo.
    #[test]
    fn undo_as_without_an_author_is_plain_lifo() {
        let mut state = State::new();
        state.doc.put(&ROOT, "a", 1).unwrap();
        state.commit_action(None, None);
        state.doc.put(&ROOT, "b", 1).unwrap();
        state.commit_action(None, None);

        state
            .undo_as(None, None, None)
            .expect("undo pops the newest");
        assert!(state.doc.get(&ROOT, "b").unwrap().is_none());
        assert!(state.doc.get(&ROOT, "a").unwrap().is_some());
    }

    /// Redo is the mirror: Bob's undone action must not be re-applied by
    /// Alice's Ctrl+Y.
    #[test]
    fn redo_as_an_author_only_reapplies_that_authors_undo() {
        let mut state = State::new();
        let alice = UserId("alice".into());
        let bob = UserId("bob".into());

        state.doc.put(&ROOT, "a", 1).unwrap();
        state.commit_action_authored(None, None, Some(alice.clone()));
        state.doc.put(&ROOT, "b", 1).unwrap();
        state.commit_action_authored(None, None, Some(bob.clone()));

        // Bob undoes his own action, putting it on the redo stack.
        state.undo_as(Some(&bob), None, None).unwrap();
        assert!(state.doc.get(&ROOT, "b").unwrap().is_none());

        // Alice hits Ctrl+Y. There is nothing of hers to redo, and Bob's
        // undone action must stay undone.
        assert!(
            state.redo_as(Some(&alice), None, None).is_none(),
            "alice has nothing to redo"
        );
        assert!(
            state.doc.get(&ROOT, "b").unwrap().is_none(),
            "alice's redo must not resurrect bob's undone action"
        );

        // Bob's own redo works.
        state
            .redo_as(Some(&bob), None, None)
            .expect("bob redoes his own");
        assert!(state.doc.get(&ROOT, "b").unwrap().is_some());
    }

    /// A local text *deletion* undone after a concurrent remote prepend must
    /// re-insert the deleted text at the correctly-shifted position.
    #[test]
    fn undo_text_delete_with_concurrent_prepend() {
        let mut state = State::new();
        let text = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        state.doc.splice_text(&text, 0, 0, "Hello World").unwrap();
        state.commit_action(None, None);
        let before_local = state.doc.get_heads();

        // LOCAL action: delete " World".
        state.doc.splice_text(&text, 5, 6, "").unwrap();
        state.commit_action(None, None);

        // Concurrent remote prepend "X".
        let mut remote = state.doc.fork_at(&before_local).unwrap();
        let rtext = remote.get(&ROOT, "body").unwrap().unwrap().1;
        remote.splice_text(&rtext, 0, 0, "X").unwrap();
        remote.commit();
        let delta = remote.save_after(&before_local);
        state.load_incremental(&delta).unwrap();
        assert_eq!(text_body(&state, &text), "XHello");

        state.undo(None, None).unwrap();
        assert_eq!(
            text_body(&state, &text),
            "XHello World",
            "deleted text restored at shifted position; remote survives"
        );
    }
}
