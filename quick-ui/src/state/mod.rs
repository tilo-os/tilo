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
}
