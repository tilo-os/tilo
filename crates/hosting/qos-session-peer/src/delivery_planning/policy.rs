//! Legacy allocating policy adapter; no resource or owner qualification.
use super::{PlanningCapacity, PlanningFailure};
use anyhow::Result;
use qos_peer_state::{PeerDeliveryPolicyError, StatePeer};
use tilo_relay::delivery_v3::commit::Digest;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

pub(super) fn inspect(
    peer: &StatePeer,
    workspace: u64,
    device: &DeviceCert,
    host: &DeviceCert,
    now: SystemTime,
) -> Result<Digest> {
    // This legacy entry is device then host; the shared entry is host then
    // device. Neither call establishes authenticated owner/session authority.
    peer.delivery_policy_unqualified(workspace, host, device, now)
        .map(|policy| policy.digest())
        .map_err(|error| {
            let failure = match error {
                PeerDeliveryPolicyError::CurrentDenied => PlanningFailure::CurrentDenied,
                PeerDeliveryPolicyError::UnsupportedPolicy => PlanningFailure::UnsupportedPolicy,
                PeerDeliveryPolicyError::Capacity => {
                    PlanningFailure::Capacity(PlanningCapacity::Bytes)
                }
                // Preserve the historical fallback for unclassified legacy
                // lookup/encoding failures. No bounded caller uses this adapter.
                PeerDeliveryPolicyError::Unavailable | PeerDeliveryPolicyError::Peer(_) => {
                    PlanningFailure::Unavailable
                }
            };
            anyhow::Error::from(failure)
        })
}
