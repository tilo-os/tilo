//! Signed invitation tickets for iroh workspace collaboration.
//!
//! An invitation binds the durable room identity to the inviter's stable iroh
//! endpoint id and its current dialing hints. The endpoint id is itself the
//! Ed25519 verification key, so no transport-specific PeerId/public-key pair is
//! duplicated in the token.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};

use crate::shell::state::{
    Collaborator, PeerOrigin, RoomId, Workspace, WorkspaceId, WorkspaceTree,
};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct PeerInviteToken {
    /// Inviter-local routing handle. The durable shared identity is `room_id`.
    pub workspace_id: u64,
    pub room_id: String,
    /// z-base-32 iroh EndpointId. The historic field name remains in v1 JSON
    /// so existing UI/debug tooling can display it without another migration.
    pub owner_peer_id: String,
    /// Fresh direct and relay dialing information for the endpoint.
    pub owner_addr: iroh::EndpointAddr,
    pub expiry_unix_seconds: u64,
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct InvitePayload<'a> {
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a iroh::EndpointAddr,
    expiry_unix_seconds: u64,
}

fn signing_payload(token: &PeerInviteToken) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(&InvitePayload {
        workspace_id: token.workspace_id,
        room_id: &token.room_id,
        owner_peer_id: &token.owner_peer_id,
        owner_addr: &token.owner_addr,
        expiry_unix_seconds: token.expiry_unix_seconds,
    })
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn make_invite_token(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    if workspace.room.is_nil() {
        return Err(std::io::Error::other(
            "workspace has no room identity to invite into",
        ));
    }
    if owner_addr.id != key.public() {
        return Err(std::io::Error::other(
            "invite address does not belong to the signing device",
        ));
    }
    let mut token = PeerInviteToken {
        workspace_id: workspace.id.raw(),
        room_id: workspace.room.to_string(),
        owner_peer_id: key.public().to_string(),
        owner_addr: owner_addr.clone(),
        expiry_unix_seconds: now_unix_seconds().saturating_add(expiry_seconds_from_now),
        signature: Vec::new(),
    };
    token.signature = key
        .sign(&signing_payload(&token).map_err(std::io::Error::other)?)
        .to_bytes()
        .to_vec();
    let json = serde_json::to_vec(&token).map_err(std::io::Error::other)?;
    Ok(B64.encode(json))
}

pub fn validate_invite_token(token_str: &str) -> Result<PeerInviteToken, String> {
    let json = B64
        .decode(token_str)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let token: PeerInviteToken =
        serde_json::from_slice(&json).map_err(|e| format!("json parse: {e}"))?;
    if token.expiry_unix_seconds < now_unix_seconds() {
        return Err("token expired".into());
    }
    match RoomId::parse(&token.room_id) {
        Some(room) if !room.is_nil() => {}
        Some(_) => return Err("invite names the unminted room".into()),
        None => return Err("invite carries no valid room id".into()),
    }
    let endpoint: iroh::EndpointId = token
        .owner_peer_id
        .parse()
        .map_err(|e| format!("endpoint id parse: {e}"))?;
    if endpoint != token.owner_addr.id {
        return Err("endpoint id does not match embedded address".into());
    }
    let signature: [u8; 64] = token
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| "signature has the wrong length".to_string())?;
    endpoint
        .verify(
            &signing_payload(&token).map_err(|e| format!("canonical encode: {e}"))?,
            &iroh_base::Signature::from_bytes(&signature),
        )
        .map_err(|_| "signature mismatch".to_string())?;
    Ok(token)
}

pub fn accept_invite_into(
    tree: &mut WorkspaceTree,
    local: WorkspaceId,
    token_str: &str,
) -> Result<RoomId, String> {
    let token = validate_invite_token(token_str)?;
    let room = RoomId::parse(&token.room_id)
        .ok_or_else(|| "invite carries no valid room id".to_string())?;
    if tree.room_of(local).is_none() {
        return Err(format!(
            "no local workspace {} to accept the invite into",
            local.raw()
        ));
    }
    tree.set_room(local, room);
    tree.pin_room_authority(room, PeerOrigin::peer_id(&token.owner_peer_id));
    let endpoint: iroh::EndpointId = token
        .owner_peer_id
        .parse()
        .map_err(|e| format!("endpoint id parse: {e}"))?;
    let session = crate::network::behaviour::peer_liveness_handle(&endpoint);
    let handle = crate::shell::state::session_handle(session);
    tree.admit_collaborator(
        local,
        Collaborator::new(handle.clone(), handle).with_session(session),
    );
    Ok(room)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: u64) -> Workspace {
        Workspace::test_default(WorkspaceId(id), format!("ws-{id}"))
    }

    fn endpoint(key: &iroh::SecretKey, port: u16) -> iroh::EndpointAddr {
        iroh::EndpointAddr::new(key.public())
            .with_ip_addr(format!("127.0.0.1:{port}").parse().unwrap())
    }

    #[test]
    fn round_trip_binds_room_key_and_dialing_address() {
        let key = iroh::SecretKey::from_bytes(&[7; 32]);
        let ws = workspace(99);
        let addr = endpoint(&key, 1234);
        let encoded = make_invite_token(&key, &ws, &addr, 3600).unwrap();
        let token = validate_invite_token(&encoded).unwrap();
        assert_eq!(token.workspace_id, 99);
        assert_eq!(token.room_id, ws.room.to_string());
        assert_eq!(token.owner_addr, addr);
        assert_eq!(token.owner_peer_id, key.public().to_string());
    }

    #[test]
    fn tampering_any_signed_route_field_is_rejected() {
        let key = iroh::SecretKey::from_bytes(&[8; 32]);
        let encoded = make_invite_token(&key, &workspace(1), &endpoint(&key, 1), 3600).unwrap();
        let bytes = B64.decode(encoded).unwrap();
        let mut token: PeerInviteToken = serde_json::from_slice(&bytes).unwrap();
        token.workspace_id = 2;
        let forged = B64.encode(serde_json::to_vec(&token).unwrap());
        assert!(
            validate_invite_token(&forged)
                .unwrap_err()
                .contains("signature")
        );
    }

    #[test]
    fn substituted_endpoint_is_rejected() {
        let owner = iroh::SecretKey::from_bytes(&[9; 32]);
        let attacker = iroh::SecretKey::from_bytes(&[10; 32]);
        let encoded = make_invite_token(&owner, &workspace(1), &endpoint(&owner, 1), 3600).unwrap();
        let bytes = B64.decode(encoded).unwrap();
        let mut token: PeerInviteToken = serde_json::from_slice(&bytes).unwrap();
        token.owner_addr = endpoint(&attacker, 2);
        let forged = B64.encode(serde_json::to_vec(&token).unwrap());
        assert!(validate_invite_token(&forged).is_err());
    }

    #[test]
    fn accepting_invite_adopts_room_and_roster_identity() {
        let key = iroh::SecretKey::from_bytes(&[11; 32]);
        let host = workspace(11);
        let encoded = make_invite_token(&key, &host, &endpoint(&key, 3), 3600).unwrap();
        let mut guest = WorkspaceTree::default();
        let guest_id = WorkspaceId(4242);
        guest
            .workspaces
            .push(Workspace::test_default(guest_id, "guest"));
        assert_eq!(
            accept_invite_into(&mut guest, guest_id, &encoded).unwrap(),
            host.room
        );
        assert_eq!(guest.room_of(guest_id), Some(host.room));
        assert_eq!(guest.workspaces[0].collaborators.len(), 1);
    }

    #[test]
    fn nil_room_cannot_be_invited() {
        let key = iroh::SecretKey::from_bytes(&[12; 32]);
        let mut ws = workspace(4);
        ws.room = RoomId::nil();
        assert!(make_invite_token(&key, &ws, &endpoint(&key, 4), 3600).is_err());
    }
}
