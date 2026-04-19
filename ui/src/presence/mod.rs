use std::collections::HashMap;

pub type UserId = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PresenceKey {
    Widget { path: Vec<String> },
    Tab { id: u64 },
    Workspace { id: u64 },
    App { app: String, sub: String },
}

#[derive(Clone, Debug)]
pub struct PeerPresence { pub user: UserId, pub color: [u8; 3], pub name: String }

#[derive(Clone, Debug, Default)]
pub struct PresenceCtx {
    peers_here: HashMap<PresenceKey, Vec<PeerPresence>>,
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
