//! Admission of direct, already committed desktop state from the pinned owner.
//! The private relay authenticates the carrier; StatePeer still verifies every
//! room change, signed record, causal prerequisite, permission and replay floor.
use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use qos_relay::{peer_actions::ActionMessage, transport_v2::EnvelopeV2};
use web_time::SystemTime;

pub fn admit(state: &StatePeer, event: &EnvelopeV2, recipient: [u8; 32], now: SystemTime)
    -> Result<Option<StatePeer>, String>
{
    let EnvelopeV2::Delivered { action, source_user } = event else { return Ok(None) };
    let ActionMessage::CommittedDesktop { packets, .. } = &action.message else { return Ok(None) };
    if !action.validate() || action.source != state.authority().device_public_key
        || *source_user != state.authority().user_public_key || action.target != recipient
        || action.room.to_string() != state.room()
    { return Err("committed desktop carrier identity mismatch".into()); }
    state.permissions(recipient, now)?;
    state.permissions(action.source, now)?;
    let mut candidate = state.clone();
    for bytes in qos_peer_state::desktop_delivery::decode(packets)? {
        let message: NeighbourMsg = postcard::from_bytes(&bytes).map_err(|e| e.to_string())?;
        if !matches!(message, NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }) {
            return Err("unsupported committed desktop packet".into());
        }
        candidate.receive_from(TransportOrigin::RelayDelta(action.source), message, now)?;
    }
    if !crate::desktop::admission(&candidate)?.complete() || candidate.incomplete_reason().is_some() {
        return Err("committed desktop prerequisites incomplete; awaiting retained state".into());
    }
    Ok(Some(candidate))
}
