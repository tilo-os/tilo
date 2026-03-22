use automerge::{AutoCommit, ObjId};

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

#[derive(Clone)]
pub struct UndoEntry {
    pub snapshot: Vec<u8>,
    pub focused_node_idx: Option<usize>, // Index in focusable_order
    pub widget_metadata: Option<String>,
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
        if self.doc.commit().is_some() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx,
                widget_metadata: metadata,
            });
            self.redo_stack.clear();
            self.last_snapshot = self.doc.save();
        }
    }

    pub fn undo(&mut self) -> Option<UndoEntry> {
        if let Some(entry) = self.undo_stack.pop() {
            let current_snapshot = self.doc.save();
            // Note: We'd ideally want to store current metadata here too for Redo
            self.redo_stack.push(UndoEntry {
                snapshot: current_snapshot,
                focused_node_idx: None, 
                widget_metadata: None,
            });
            
            if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
                self.doc = new_doc;
                self.last_snapshot = entry.snapshot.clone();
                return Some(entry);
            }
        }
        None
    }

    pub fn redo(&mut self) -> Option<UndoEntry> {
        if let Some(entry) = self.redo_stack.pop() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx: None,
                widget_metadata: None,
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
}
