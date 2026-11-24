//! The server an invitation names, read from the invitation alone: how a page reaches a server
//! that has no web origin it could ask.

use tilo_server_protocol::web::PageHost;
use tilo_user_identity::RevocationList;
use wasm_bindgen::prelude::*;

use crate::peer::js_error;

/// The server `invitation` (the signed invitation's bytes) admits into, as the invitation itself
/// names it: the server's own signed iroh address and certificate in it, checked under the
/// invitation's signature. A server reached only over iroh (behind a NAT, through a packet relay)
/// has no web origin to ask, and a page dials it with this alone. Answers JSON
/// `{info, endpoint, offer}`: `info` as `hostInfo` answers it, the server's endpoint id, and
/// `offer` as [`inspect_invitation`] does but from what the invitation signs (the workspace's name
/// and its people are the server's to say, once in).
///
/// # Errors
///
/// The invitation is not a valid one now, or names no server to dial.
#[wasm_bindgen(js_name = invitedServer)]
pub fn invited_server(invitation: &[u8]) -> Result<String, JsError> {
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
    let verified = tilo_workspace_join::decode_and_verify_invitation(
        invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(js_error)?;
    let grant = verified.grant();
    let servers: Vec<WorkspaceDeviceHintV1> = grant
        .transport
        .iter()
        .filter_map(|hint| WorkspaceDeviceHintV1::from_transport_hint(hint).ok())
        .filter(|server| server.workspace_id == grant.workspace_id)
        .collect();
    // Only a server that holds the workspace's keys lets a new member in.
    let server = servers
        .iter()
        .find(|server| server.holds_keys)
        .or_else(|| servers.first())
        .ok_or_else(|| js_error("this invitation names no server to dial"))?;
    let info = tilo_server_protocol::web::ServerInfo {
        server_cert: server.device_certificate.clone(),
        address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.endpoint_addr.clone(),
            relay_url: server
                .packet_relay_url
                .parse::<iroh_base::RelayUrl>()
                .ok()
                .map(|url| url.to_string()),
            web: None,
        }),
        origin: None,
    };
    let offer = serde_json::json!({
        "kind": "invitation", "workspace": grant.workspace_id.to_string(),
        "workspaceId": grant.workspace_id.to_string(), "name": null, "role": grant.role,
        "expiresAtNs": grant.expires_at_ns.to_string(), "members": null,
        "administrator": tilo_workspace_join::makes_administrator(invitation), "owner": false, "waitList": false,
        "inviter": verified.inviter().map(|inviter| serde_json::json!({
            "name": inviter.name, "userPublicKey": hex::encode(inviter.user_public_key),
        })),
    });
    Ok(serde_json::json!({
        "info": serde_json::to_value(PageHost::from(info)).map_err(js_error)?,
        "endpoint": server.endpoint_addr.id.to_string(),
        "offer": offer,
    })
    .to_string())
}
