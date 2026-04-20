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

    pub fn undo(&mut self) -> Option<UndoEntry> {
        if let Some(entry) = self.undo_stack.pop() {
            let current_snapshot = self.doc.save();
            // Note: We'd ideally want to store current metadata here too for Redo
            self.redo_stack.push(UndoEntry {
                snapshot: current_snapshot,
                focused_node_idx: None,
                widget_metadata: None,
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
    pub fn undo_for_author(&mut self, author: &UserId) -> Option<UndoEntry> {
        let idx = self.undo_stack.iter().rposition(|e| {
            matches!(&e.author, Some(a) if a == author)
        })?;
        let entry = self.undo_stack.remove(idx);
        let current_snapshot = self.doc.save();
        self.redo_stack.push(UndoEntry {
            snapshot: current_snapshot,
            focused_node_idx: None,
            widget_metadata: None,
            author: entry.author.clone(),
        });
        if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
            self.doc = new_doc;
            self.last_snapshot = entry.snapshot.clone();
            return Some(entry);
        }
        None
    }

    pub fn redo(&mut self) -> Option<UndoEntry> {
        if let Some(entry) = self.redo_stack.pop() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx: None,
                widget_metadata: None,
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
