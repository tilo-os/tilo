use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId, PatchAction, ReadDoc, Value};
use std::collections::HashMap;
use crate::permissions::UserId;

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

/// One reversible action on the undo / redo stacks.
///
/// **Collab-safe, change-based undo.** Instead of snapshotting the whole
/// document (which, on undo, would replace the live `AutoCommit` and silently
/// drop any concurrently-merged remote ops), an entry records only the
/// document *heads* that bracket the local action: `before` (heads just prior
/// to the action) and `after` (heads just after it committed). Undo recomputes
/// the inverse of that action — `doc.diff(after, before)` — and writes it back
/// as a brand-new forward change merged into the live document. Because it is a
/// *new* change rather than a state replacement, every concurrent remote edit
/// that arrived in between is preserved; only the local action is reverted.
///
/// `focused_node_idx` / `widget_metadata` / `author` are unchanged from the
/// previous design — callers (`App::apply_undo_entry`, per-author undo) read
/// these to restore the caret/selection and to isolate multi-user undo.
#[derive(Clone)]
pub struct UndoEntry {
    /// Document heads immediately before the action was applied.
    pub before: Vec<ChangeHash>,
    /// Document heads immediately after the action committed.
    pub after: Vec<ChangeHash>,
    pub focused_node_idx: Option<usize>, // Index in focusable_order
    pub widget_metadata: Option<String>,
    pub author: Option<UserId>,
}

/// The unified state object that holds our CRDT document.
pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    pub collab: CollaborationState,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// Heads as of the last commit boundary — the `before` for the next action.
    last_heads: Vec<ChangeHash>,
}

impl State {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let last_heads = doc.get_heads();
        Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_heads,
        }
    }

    /// Commit the current transaction and push a change-based undo entry
    /// recording the heads that bracket this action.
    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    /// Commit the current transaction and push a change-based undo entry,
    /// attributing it to `author` so multi-user undo can isolate users.
    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) {
        let before = self.last_heads.clone();
        // Stamp the change with wall-clock time. `AutoCommit::commit` leaves
        // automerge's advisory timestamp at zero, which is why a history pane
        // built over a document committed that way can show authorship but not
        // *when* — and why a date-range filter over it matches nothing.
        if qdoc::history::commit_stamped(&mut self.doc).is_some() {
            let after = self.doc.get_heads();
            self.undo_stack.push(UndoEntry {
                before,
                after,
                focused_node_idx,
                widget_metadata: metadata,
                author,
            });
            self.redo_stack.clear();
            self.last_heads = self.doc.get_heads();
        }
    }

    /// Inspect the most recently pushed undo entry without popping it.
    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.last()
    }

    /// Apply the inverse of the change spanning `[entry.before, entry.after]`
    /// as a NEW forward change merged into the live document, and return the
    /// `[before, after]` heads of that newly-written inverse change so the
    /// opposite stack can later invert *it* (redo, or re-undo).
    ///
    /// Concurrent remote ops merged into `self.doc` between `entry.after` and
    /// now are untouched: we only re-derive and rewrite the props/elements the
    /// local action itself changed.
    fn apply_inverse(&mut self, entry: &UndoEntry) -> (Vec<ChangeHash>, Vec<ChangeHash>) {
        let inverse_before = self.doc.get_heads();
        // Patches describing how to get from the post-action state back to the
        // pre-action state — i.e. the inverse of the local action.
        let patches = self.doc.diff(&entry.after, &entry.before);
        // Recreated objects get a fresh ExId; map the patch's (old) ObjId to it
        // so nested child patches target the new object.
        let mut remap: HashMap<ObjId, ObjId> = HashMap::new();
        for patch in patches {
            let target = remap.get(&patch.obj).cloned().unwrap_or(patch.obj.clone());
            match patch.action {
                PatchAction::PutMap { key, value, .. } => match value.0 {
                    Value::Scalar(s) => {
                        let _ = self.doc.put(&target, key, s.into_owned());
                    }
                    Value::Object(ot) => {
                        if let Ok(new_id) = self.doc.put_object(&target, key, ot) {
                            remap.insert(value.1, new_id);
                        }
                    }
                },
                PatchAction::PutSeq { index, value, .. } => {
                    // The element still exists post-action; anchor at `after`.
                    let li = self.live_index_at(&target, index, &entry.after);
                    match value.0 {
                        Value::Scalar(s) => {
                            let _ = self.doc.put(&target, li, s.into_owned());
                        }
                        Value::Object(ot) => {
                            if let Ok(new_id) = self.doc.put_object(&target, li, ot) {
                                remap.insert(value.1, new_id);
                            }
                        }
                    }
                }
                PatchAction::Insert { index, values } => {
                    // Re-inserting elements the local action deleted; the
                    // boundary is expressed in `before` coordinates.
                    let mut li = self.live_insert_boundary(&target, index, &entry.before);
                    for (val, old_id, _) in values.into_iter() {
                        match val {
                            Value::Scalar(s) => {
                                let _ = self.doc.insert(&target, li, s.clone().into_owned());
                            }
                            Value::Object(ot) => {
                                if let Ok(new_id) = self.doc.insert_object(&target, li, *ot) {
                                    remap.insert(old_id.clone(), new_id);
                                }
                            }
                        }
                        li += 1;
                    }
                }
                PatchAction::SpliceText { index, value, .. } => {
                    // Re-inserting text the local action deleted.
                    let li = self.live_insert_boundary(&target, index, &entry.before);
                    let _ = self.doc.splice_text(&target, li, 0, &value.make_string());
                }
                PatchAction::Increment { prop, value } => {
                    let _ = self.doc.increment(&target, prop, value);
                }
                PatchAction::DeleteMap { key } => {
                    let _ = self.doc.delete(&target, key);
                }
                PatchAction::DeleteSeq { index, length } => {
                    // Removing elements the local action inserted; they exist
                    // post-action, so each position is anchored at `after`.
                    for _ in 0..length {
                        let li = self.live_index_at(&target, index, &entry.after);
                        let _ = self.doc.delete(&target, li);
                    }
                }
                // A conflict marker carries no content to revert, and marks
                // (text formatting spans) are not produced by the editor's
                // undo-tracked actions; both are safely ignored.
                PatchAction::Conflict { .. } | PatchAction::Mark { .. } => {}
            }
        }
        let _ = qdoc::history::commit_stamped(&mut self.doc);
        let inverse_after = self.doc.get_heads();
        self.last_heads = inverse_after.clone();
        (inverse_before, inverse_after)
    }

    /// Rewrite the document to the content it had at `target_heads`, as a NEW
    /// forward change.
    ///
    /// This is "restore to this version" from the history scrubber. It takes
    /// exactly the same route as undo — derive the patches that lead from the
    /// current state to the target state, then *write them* — so history is
    /// appended to, never rewritten. Three consequences the surrounding
    /// features depend on:
    ///
    /// * The restore shows up in the timeline as its own new entry, authored by
    ///   whoever performed it. The versions it rolled past are still there.
    /// * Remote edits merged in while the user was scrubbing survive, because
    ///   only the properties and elements that actually differ are rewritten.
    /// * The restore is itself undoable — it lands on the undo stack like any
    ///   other action.
    ///
    /// Returns `true` when something changed. A restore to the state the
    /// document is already in is a no-op and returns `false`.
    pub fn restore_to_heads(
        &mut self,
        target_heads: &[ChangeHash],
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) -> bool {
        let current = self.doc.get_heads();
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
        let (inv_before, inv_after) = self.apply_inverse(&entry);
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

    /// Translate a sequence index expressed as-of `anchor_heads` into its
    /// position in the live document, so concurrent inserts/deletes shift it
    /// correctly. Used when the referenced element still exists.
    fn live_index_at(
        &self,
        obj: &ObjId,
        index: usize,
        anchor_heads: &[ChangeHash],
    ) -> usize {
        match self.doc.get_cursor(obj, index, Some(anchor_heads)) {
            Ok(cursor) => self
                .doc
                .get_cursor_position(obj, &cursor, None)
                .unwrap_or(index),
            Err(_) => index,
        }
    }

    /// Translate an *insertion boundary* expressed as-of `anchor_heads` into the
    /// live document. The element originally at `index` may itself have been
    /// deleted by the action we're reverting (which would trip an automerge
    /// fast/slow cursor-seek consistency check), so we anchor on the surviving
    /// element just before the boundary and insert after it.
    fn live_insert_boundary(
        &self,
        obj: &ObjId,
        index: usize,
        anchor_heads: &[ChangeHash],
    ) -> usize {
        if index == 0 {
            return 0;
        }
        match self.doc.get_cursor(obj, index - 1, Some(anchor_heads)) {
            Ok(cursor) => self
                .doc
                .get_cursor_position(obj, &cursor, None)
                .map(|p| p + 1)
                .unwrap_or(index),
            Err(_) => index,
        }
    }

    /// Undo the most recent action by writing its inverse as a new forward
    /// change (preserving concurrent remote edits). Callers pass the *current*
    /// focus index and widget metadata so the redo entry carries the caret /
    /// focus state at the moment undo fired — otherwise Ctrl+Shift+Z would
    /// rehydrate the text body but leave the caret at offset 0.
    pub fn undo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        let entry = self.undo_stack.pop()?;
        let (inv_before, inv_after) = self.apply_inverse(&entry);
        // Redo re-applies the original action = invert the inverse change.
        self.redo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
    }

    /// Multi-user aware undo. Pops the topmost entry whose `author` equals
    /// `author`, writing its inverse as a new forward change. Entries from
    /// other users are preserved in the stack so that one collaborator's undo
    /// never reverts another's work — and, because the revert is a new change
    /// rather than a snapshot reload, neither does it clobber their committed
    /// ops in the live document.
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
        let idx = self.undo_stack.iter().rposition(|e| {
            matches!(&e.author, Some(a) if a == author)
        })?;
        let entry = self.undo_stack.remove(idx);
        let (inv_before, inv_after) = self.apply_inverse(&entry);
        self.redo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
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
        let entry = self.redo_stack.pop()?;
        let (inv_before, inv_after) = self.apply_inverse(&entry);
        self.undo_stack.push(UndoEntry {
            before: inv_before,
            after: inv_after,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        Some(entry)
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
        let last_heads = doc.get_heads();
        Self {
            doc,
            root: automerge::ROOT,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_heads,
        }
    }

    /// Restores an application state from a serialized byte vector.
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let last_heads = doc.get_heads();
        Ok(Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_heads,
        })
    }

    /// Apply an incremental CRDT patch (bytes produced by
    /// `AutoCommit::save_incremental` on another peer) into this document.
    /// Returns the number of ops applied on success.
    ///
    /// This is how `CollabEvent::CRDTIncremental` is actually merged on the
    /// receiver side — without it, incremental deltas broadcast by one peer
    /// would never reach another peer's CRDT.
    pub fn load_incremental(
        &mut self,
        data: &[u8],
    ) -> Result<usize, automerge::AutomergeError> {
        let n = self.doc.load_incremental(data)?;
        // Keep the undo baseline aligned with the new, merged document state
        // so the *next* local action brackets against a complete picture.
        //
        // NOTE: existing undo entries keep their original `before`/`after`
        // heads. Those heads remain valid addresses into the (append-only)
        // change graph after a merge, so undoing an earlier local action still
        // reverts exactly that action as a new change — leaving the just-merged
        // remote ops intact. This is precisely the collab-safety the old
        // snapshot-reload design lacked.
        self.last_heads = self.doc.get_heads();
        Ok(n)
    }

    /// Serialize the changes made since the last save/save_incremental call.
    /// The returned bytes can be fed into another peer's `load_incremental`
    /// to replay just the delta — the basic building block of efficient
    /// CRDT sync.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{transaction::Transactable, ReadDoc, ScalarValue};

    /// A single commit_action → undo → redo round-trip must preserve the
    /// focus index and widget metadata captured on the stacks — that's
    /// what restores the caret in `EditorApp` after Ctrl+Shift+Z. Pre-fix
    /// the redo stack was always pushed with `None, None`, so the entry
    /// returned by `redo(...)` lost both fields.
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

    /// THE HEADLINE TEST. A committed local action, then a *concurrent remote
    /// change* merged in, then `undo()`. The remote change MUST survive and only
    /// the local action is reverted. With the old snapshot-reload undo this
    /// failed: undo replaced the whole doc with a pre-merge snapshot, dropping
    /// the remote edit entirely.
    #[test]
    fn undo_preserves_concurrent_remote_edit() {
        let mut state = State::new();
        let text = state
            .doc
            .put_object(&ROOT, "body", ObjType::Text)
            .unwrap();
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
        assert_eq!(text_body(&state, &text), "XHello World", "precondition merge");

        // UNDO the local action.
        state.undo(None, None).expect("undo pops the local action");

        // Local " World" reverted; remote "X" survives. With snapshot-reload
        // this would have been "Hello" (remote dropped).
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
    /// pre/post values (behaviour identical to the old design).
    #[test]
    fn single_user_undo_redo_roundtrip() {
        let mut state = State::new();
        state.doc.put(&ROOT, "k", ScalarValue::Str("a".into())).unwrap();
        state.commit_action(None, None);
        state.doc.put(&ROOT, "k", ScalarValue::Str("b".into())).unwrap();
        state.commit_action(None, None);

        let val = |s: &State| s.doc.get(&ROOT, "k").unwrap().unwrap().0.to_str().unwrap().to_string();
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
