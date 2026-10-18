use crate::NeighbourMsg;

/// Extract the current-chain pin from a bootstrap returned by the exact
/// authenticated room governor. This is **not** valid for relay replay, gossip,
/// or a cached/unverified response: the caller must first authenticate the join
/// exchange and its room authority independently of these messages.
///
/// Kept in the existing governance-message wire vocabulary, so ordinary rooms
/// and older stored bootstrap layouts do not acquire a new Postcard field.
/// # Errors
/// Refuses duplicate baselines, foreign room/signers, malformed framing and
/// signatures. A legacy bootstrap without a replacement returns `None`.
pub fn sanitized_anchor_from_authenticated_bootstrap(
    room: &str,
    authority: &tilo_user_identity::DeviceCert,
    governance_messages: &[Vec<u8>],
) -> Result<Option<tilo_peer_state::SanitizedRoomBaselineAnchor>, String> {
    let mut pin = None;
    for bytes in governance_messages {
        let (message, tail) =
            postcard::take_from_bytes::<NeighbourMsg>(bytes).map_err(|e| e.to_string())?;
        if !tail.is_empty() {
            return Err("trailing bootstrap governance bytes".into());
        }
        if let NeighbourMsg::SanitizedRoomBaseline {
            room: named,
            baseline,
        } = message
        {
            if pin.is_some() || named != room {
                return Err("duplicate or foreign bootstrap baseline".into());
            }
            let (baseline, tail) =
                postcard::take_from_bytes::<tilo_peer_state::SanitizedRoomBaseline>(&baseline)
                    .map_err(|e| e.to_string())?;
            if !tail.is_empty() {
                return Err("trailing bootstrap baseline bytes".into());
            }
            let anchor = baseline.anchor();
            baseline.verify_anchored_document(room, authority.device_public_key, &anchor)?;
            pin = Some(anchor);
        }
    }
    Ok(pin)
}
