//! LIVE_V1 lane E: display identity for live publishers. A publisher is the
//! device key hex of its namespace; the overlay labels it with the member's
//! governed display name and colours it by the member's user key, so every
//! device of one person shares one stable colour. The structural owner is not
//! part of the governed collaborator projection; its certified devices are
//! added with the owner's governed label (or no name: the overlay then says
//! "Owner").
use super::*;
use crate::live_view::{LiveMember, live_member_rows, owner_display_name};

/// `[{device, user, name, owner}]` for every active member device of the room.
#[wasm_bindgen]
pub fn peer_live_members(id: u32) -> Result<String, JsValue> {
    let peer = PEERS
        .with(|p| p.borrow().get(&id).cloned())
        .filter(|peer| !peer.closed.get())
        .ok_or_else(|| JsValue::from_str("closed Peer"))?;
    let state = peer.state.borrow();
    let now = SystemTime::now();
    let members = state
        .governed_members(now)
        .map_err(|e| JsValue::from_str(&e))?;
    let collaborators = members
        .iter()
        .filter(|member| !member.removed)
        .flat_map(|member| {
            member
                .active_device_keys
                .iter()
                .map(move |device| LiveMember {
                    device: *device,
                    user: member.user_public_key,
                    name: member.display_name.clone(),
                    owner: false,
                })
        })
        .collect();
    let owner = state.owner_user_public_key();
    let names = state
        .admitted_room()
        .map(|doc| qos_peer_state::room::governance_of(Some(doc)).2)
        .unwrap_or_default();
    let revoked = state
        .revocations()
        .into_iter()
        .map(|entry| entry.revoked_device_public_key)
        .collect::<BTreeSet<_>>();
    let owner_devices = state
        .certificates()
        .into_iter()
        .filter(|cert| cert.user_public_key == owner && !revoked.contains(&cert.device_public_key))
        .map(|cert| LiveMember {
            device: cert.device_public_key,
            user: owner,
            name: owner_display_name(
                names
                    .get(&crate::live_view::hex(&cert.device_public_key))
                    .map(String::as_str)
                    .or(Some(cert.device_label.as_str())),
            ),
            owner: true,
        })
        .collect();
    serde_json::to_string(&live_member_rows(collaborators, owner_devices))
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
