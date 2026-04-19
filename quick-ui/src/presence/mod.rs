use std::collections::HashMap;

mod key;
pub use key::{PresenceKey, PathSegment, SelectionRange};

pub type UserId = String;

#[derive(Clone, Debug)]
pub struct PeerPresence { pub user: UserId, pub color: [u8; 3], pub name: String }

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct PresenceCtx {
    peers_here: HashMap<PresenceKey, Vec<PeerPresence>>,
    pub remote_cursors: Vec<RemoteCursor>,
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }
    pub fn peers_for(&self, key: &PresenceKey) -> &[PeerPresence] {
        self.peers_here.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn insert_peer(&mut self, key: PresenceKey, peer: PeerPresence) {
        self.peers_here.entry(key).or_default().push(peer);
    }
}
