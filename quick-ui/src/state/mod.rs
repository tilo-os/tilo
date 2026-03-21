use automerge::{AutoCommit, ObjId};

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

/// The unified state object that holds our CRDT document.
pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    pub collab: CollaborationState,
}

impl State {
    pub fn new() -> Self {
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        Self { 
            doc, 
            root,
            collab: CollaborationState::new(),
        }
    }

    /// Serializes the entire application CRDT state into a byte vector.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Restores an application state from a serialized byte vector.
    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        Ok(Self {
            doc,
            root,
            collab: CollaborationState::new(),
        })
    }
}
