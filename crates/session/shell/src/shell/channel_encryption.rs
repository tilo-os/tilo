//! End-to-end encryption of collaboration channels on this device. The
//! service is `tilo_action_broker::channel_encryption`'s; its governance
//! messages travel over this session's room transport.
pub use tilo_action_broker::channel_encryption::*;

impl GovernanceTransport for super::network::ShellNetwork {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        self.publish_channel_governance(message)
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        self.inbound_channel_governance_handle()
            .lock()
            .map(|mut inbound| std::mem::take(&mut *inbound))
            .unwrap_or_default()
    }
}
