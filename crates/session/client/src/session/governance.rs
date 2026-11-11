//! The channels' workspace transport over the host session: the channel
//! governance this page says waits here until the host takes it for a
//! workspace, and goes out wrapped as the page's own retained frame.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_relay_client::CoeditPayload;
use tilo_relay_protocol::Envelope;
use tilo_relay_session::RelayClient;

use super::Task;

/// The channels' workspace transport over the host session: what they say
/// waits here for the session to send it, newest per channel, until the host
/// admitted the session to its workspace.
#[derive(Default)]
pub(super) struct HostGovernance {
    pub(super) inbound: Mutex<Vec<tilo_channels::InboundChannelGovernance>>,
    outbound: Mutex<BTreeMap<(u64, [u8; 16]), neighbour_wire::NeighbourMsg>>,
}

impl HostGovernance {
    /// What waits for the workspaces `admitted` names.
    pub(super) fn take_for(
        &self,
        admitted: impl Fn(u64) -> bool,
    ) -> Vec<(u64, neighbour_wire::NeighbourMsg)> {
        let outbound = std::mem::take(&mut *self.outbound.lock_recover());
        let (due, waiting): (BTreeMap<_, _>, BTreeMap<_, _>) = outbound
            .into_iter()
            .partition(|((workspace_id, _), _)| admitted(*workspace_id));
        self.outbound.lock_recover().extend(waiting);
        due.into_iter()
            .map(|((workspace_id, _), message)| (workspace_id, message))
            .collect()
    }
}

impl tilo_channels::GovernanceTransport for HostGovernance {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        use neighbour_wire::NeighbourMsg;
        let (workspace, channel) = match &message {
            NeighbourMsg::MlsAccessRequest {
                workspace, request, ..
            } => (workspace, request.channel),
            NeighbourMsg::MlsRoutedAccessRequest { room, request, .. } => (room, request.channel),
            // A reader commits nothing.
            _ => return false,
        };
        let Ok(workspace_id) = tilo_relay_protocol::room_workspace_id(workspace) else {
            return false;
        };
        self.outbound
            .lock_recover()
            .insert((workspace_id, channel.0.into_bytes()), message);
        true
    }

    fn take_inbound(&self) -> Vec<tilo_channels::InboundChannelGovernance> {
        std::mem::take(&mut *self.inbound.lock_recover())
    }
}

/// `message`, signed governance of `workspace_id`, wrapped for the host.
fn governance_envelope(
    device: [u8; 32],
    workspace_id: u64,
    message: &neighbour_wire::NeighbourMsg,
) -> Option<Envelope> {
    let (channel, _) = neighbour_wire::retention(message, false)?;
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: device,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_allocvec(message).ok()?,
        }
        .encode()
        .ok()?,
        signature: [0; 64],
    })
}

impl Task {
    /// Send the governance the channels said for every workspace the host
    /// admitted this session to or began replaying to it; the rest waits
    /// until it does. Returns `false` when the session could not take an
    /// envelope.
    pub(super) fn send_governance(&self, client: &RelayClient) -> bool {
        // A workspace whose replay has begun takes the page's channel
        // governance: its access requests go out as soon as the replay's
        // governance names the page, so its keys arrive while the rest of the
        // workspace is still on its way, rather than a whole replay late.
        let admitted = |workspace_id: u64| {
            self.begun.contains(&workspace_id)
                || self
                    .reports
                    .read(|status| status.workspaces.get(&workspace_id).copied())
                    == Some(true)
        };
        let device = self.config.cert.device_public_key;
        let mut delivered = true;
        for (workspace_id, message) in self.governance.take_for(admitted) {
            if let Some(envelope) = governance_envelope(device, workspace_id, &message) {
                delivered &= client.send(envelope).is_ok();
            }
        }
        delivered
    }
}
