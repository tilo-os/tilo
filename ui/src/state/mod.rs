use automerge::{AutoCommit, ObjId};
use crate::permissions::UserId;

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

#[derive(Clone)]
pub struct UndoEntry {
    pub snapshot: Vec<u8>,
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
    last_snapshot: Vec<u8>,
}

impl State {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let last_snapshot = doc.save();
        Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot,
        }
    }

    /// Commit the current transaction and add the PREVIOUS snapshot to the undo stack.
    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    /// Commit the current transaction and add the PREVIOUS snapshot to the undo stack,
    /// attributing the entry to `author` so multi-user undo can isolate users.
    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) {
        if self.doc.commit().is_some() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx,
                widget_metadata: metadata,
                author,
            });
            self.redo_stack.clear();
            self.last_snapshot = self.doc.save();
        }
    }

    /// Inspect the most recently pushed undo entry without popping it.
    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.last()
    }

    /// Undo the most recent action. Callers pass the *current* focus index
    /// and widget metadata so the opposite-stack entry (pushed onto
    /// `redo_stack`) carries the caret/focus state at the moment undo
    /// fired — otherwise Ctrl+Shift+Z would rehydrate the text body but
    /// leave the caret at offset 0.
    pub fn undo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        if let Some(entry) = self.undo_stack.pop() {
            let current_snapshot = self.doc.save();
            self.redo_stack.push(UndoEntry {
                snapshot: current_snapshot,
                focused_node_idx: current_focused_idx,
                widget_metadata: current_metadata,
                author: entry.author.clone(),
            });

            if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
                self.doc = new_doc;
                self.last_snapshot = entry.snapshot.clone();
                return Some(entry);
            }
        }
        None
    }

    /// Multi-user aware undo. Pops the topmost entry whose `author` equals
    /// `author`. Entries from other users are preserved in the stack so
    /// that one collaborator's undo never reverts another's work.
    ///
    /// `current_focused_idx` / `current_metadata` are captured on the
    /// redo stack so a subsequent redo can restore the caret — same
    /// rationale as [`undo`].
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
        let current_snapshot = self.doc.save();
        self.redo_stack.push(UndoEntry {
            snapshot: current_snapshot,
            focused_node_idx: current_focused_idx,
            widget_metadata: current_metadata,
            author: entry.author.clone(),
        });
        if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
            self.doc = new_doc;
            self.last_snapshot = entry.snapshot.clone();
            return Some(entry);
        }
        None
    }

    /// Redo the most recently undone action. `current_focused_idx` /
    /// `current_metadata` are captured on the undo stack so a subsequent
    /// undo can park the caret back where it was pre-redo.
    pub fn redo(
        &mut self,
        current_focused_idx: Option<usize>,
        current_metadata: Option<String>,
    ) -> Option<UndoEntry> {
        if let Some(entry) = self.redo_stack.pop() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx: current_focused_idx,
                widget_metadata: current_metadata,
                author: entry.author.clone(),
            });

            if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
                self.doc = new_doc;
                self.last_snapshot = entry.snapshot.clone();
                return Some(entry);
            }
        }
        None
    }

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Restores an application state from a serialized byte vector.
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let last_snapshot = doc.save();
        Ok(Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot,
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
        // so the *next* local action can snapshot against a complete picture.
        self.last_snapshot = self.doc.save();
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
}
