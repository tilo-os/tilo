//! What a device that links to this one is handed of a workspace's key groups.
//!
//! A group moves by frames its members publish, each once. A host that keeps
//! a group's newest frame replays it to a member that comes back; a device
//! its members reach a workspace through over links of their own keeps none
//! for them, so it hands each what its key service holds as the link starts.

use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;

use super::{SharedKeys, WorkspaceKeys};

impl WorkspaceKeys {
    /// The governance a device that links to this one in `workspace` is handed,
    /// so it is current in every group of the workspace this device is in, its reader sets'
    /// included:
    /// each group's genesis and head, as this device holds them
    /// ([`key_group::GroupPeer::current_frames`]).
    ///
    /// What this device publishes reaches the members linked to it then, and a
    /// host that keeps a group's newest frame hands it to a member that comes
    /// back. The members of a workspace a device hosts itself reach it over
    /// their own links, where nothing keeps that frame: a member whose link
    /// came up after a commit never heard of it, sealed under keys the others
    /// had moved past, and was turned away from the workspace's live session for
    /// as long as nobody committed again.
    pub fn current_governance(&mut self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        let mut messages = Vec::new();
        for entry in self
            .peers
            .values()
            .filter(|entry| self.directory.carrier(entry.workspace) == workspace)
        {
            for frame in entry.peer.current_frames() {
                self.delivery = self.delivery.wrapping_add(1);
                messages.push(NeighbourMsg::MlsGovernance {
                    workspace: workspace.to_string(),
                    frame: Box::new(frame),
                    delivery: self.delivery,
                });
            }
        }
        messages
    }
}

impl SharedKeys {
    /// See [`WorkspaceKeys::current_governance`]; nothing when the service is
    /// unavailable after a poisoned lock.
    #[must_use]
    pub fn current_governance(&self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        self.with_service(|service| service.current_governance(workspace))
            .unwrap_or_default()
    }
}
