//! Multi-recipient surface-sharing state for one workspace.
//!
//! The wire shape remains a map from Wayland surface id to peer ids; this
//! component owns the invariants around adding and removing recipients.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct SharedSurfaces(HashMap<u64, HashSet<u64>>);

impl<'de> Deserialize<'de> for SharedSurfaces {
    /// Surface ids are JSON object keys; accept them however the map was
    /// buffered (see [`crate::serde_ids`]).
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        crate::serde_ids::deserialize_id_map(deserializer).map(Self)
    }
}

impl SharedSurfaces {
    pub fn share(&mut self, surface_id: u64, peer_id: u64) {
        self.0.entry(surface_id).or_default().insert(peer_id);
    }

    pub fn unshare(&mut self, surface_id: u64) -> Option<HashSet<u64>> {
        self.0.remove(&surface_id)
    }

    pub fn unshare_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        let Some(peers) = self.0.get_mut(&surface_id) else {
            return false;
        };
        let removed = peers.remove(&peer_id);
        if peers.is_empty() {
            self.0.remove(&surface_id);
        }
        removed
    }

    pub fn contains(&self, surface_id: u64) -> bool {
        self.0
            .get(&surface_id)
            .is_some_and(|peers| !peers.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate the surfaces and their explicit recipients. The compositor
    /// uses this read-only view to reconcile live encoder sessions with the
    /// replicated workspace state.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &HashSet<u64>)> {
        self.0.iter().map(|(surface, peers)| (*surface, peers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Workspace, WorkspaceId};

    #[test]
    fn shared_surface_registry_adds_removes_and_keeps_its_shape() {
        {
            let mut shares = SharedSurfaces::default();
            shares.share(7, 42);
            shares.share(7, 99);
            shares.share(7, 42);

            assert_eq!(shares.0[&7], HashSet::from([42, 99]));
            assert!(shares.contains(7));
        }
        {
            let mut shares = SharedSurfaces::default();
            shares.share(7, 42);
            shares.share(7, 99);

            assert!(shares.unshare_peer(7, 42));
            assert_eq!(shares.0[&7], HashSet::from([99]));
            assert!(!shares.unshare_peer(7, 42));
        }
        {
            let mut shares = SharedSurfaces::default();
            shares.share(7, 42);

            assert!(shares.unshare_peer(7, 42));
            assert!(!shares.contains(7));
            assert!(shares.is_empty());

            shares.share(8, 42);
            shares.share(8, 99);
            assert_eq!(shares.unshare(8), Some(HashSet::from([42, 99])));
            assert!(!shares.contains(8));
        }
        {
            let mut shares = SharedSurfaces::default();
            shares.share(11, 100);

            let json = serde_json::to_value(&shares).expect("serialize");
            assert_eq!(json, serde_json::json!({ "11": [100] }));
            assert_eq!(
                serde_json::from_value::<SharedSurfaces>(json)
                    .expect("deserialize")
                    .0[&11],
                HashSet::from([100])
            );
        }
        {
            let mut workspace = Workspace::new(WorkspaceId(1), "Demo");
            workspace.share_surface(7, 42);
            workspace.share_surface(7, 99);
            assert!(workspace.is_surface_shared(7));
            assert!(workspace.unshare_surface_from_peer(7, 42));
            assert_eq!(workspace.unshare_surface(7), Some(HashSet::from([99])));
            assert!(!workspace.is_surface_shared(7));
        }
    }
}
