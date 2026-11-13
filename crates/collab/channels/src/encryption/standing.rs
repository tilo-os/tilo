//! What a device that links to this one is handed of a workspace's channels.
//!
//! A channel's group moves by frames its members publish, each once. A host
//! that keeps a channel's newest frame replays it to a member that comes back;
//! a device its members reach a workspace through over links of their own
//! keeps none for them, so it hands each what its channel service holds as the
//! link starts.

use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;

use super::{ChannelEncryption, ChannelKeyAccess};

impl ChannelEncryption {
    /// The governance a device that links to this one in `workspace` is handed,
    /// so it is current in every channel of the workspace this device is in:
    /// each channel's genesis and head, as this device holds them
    /// ([`channel_crypto::ChannelPeer::standing`]).
    ///
    /// What this device publishes reaches the members linked to it then, and a
    /// host that keeps a channel's newest frame hands it to a member that
    /// comes back. The members of a workspace a device hosts itself reach it
    /// over their own links, where nothing keeps that frame: a member whose
    /// link came up after a commit never heard of it, sealed under keys the
    /// others had moved past, and was turned away from the workspace's live
    /// plane for as long as nobody committed again.
    pub fn standing_governance(&mut self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        let mut messages = Vec::new();
        for entry in self
            .peers
            .values()
            .filter(|entry| entry.workspace == workspace)
        {
            for frame in entry.peer.standing() {
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

impl ChannelKeyAccess {
    /// See [`ChannelEncryption::standing_governance`]; nothing when the
    /// service is unavailable after a poisoned lock.
    #[must_use]
    pub fn standing_governance(&self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        self.run(|service| service.standing_governance(workspace))
            .unwrap_or_default()
    }
}
