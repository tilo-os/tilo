//! Signed invitation tickets for iroh workspace collaboration.
//!
//! An invitation binds the durable room identity to the inviter's stable iroh
//! endpoint id and its current dialing hints. The endpoint id is itself the
//! Ed25519 verification key, so no transport-specific PeerId/public-key pair is
//! duplicated in the token.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use qos_user_identity::DeviceCert;
use serde::{Deserialize, Serialize};

use crate::shell::state::{
    Collaborator, PeerOrigin, RoomId, Workspace, WorkspaceId, WorkspaceTree,
};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct PeerInviteToken {
    /// Inviter-local routing handle. The durable shared identity is `room_id`.
    pub workspace_id: u64,
    pub room_id: String,
    /// z-base-32 iroh `EndpointId`. The historic field name remains in v1 JSON
    /// so existing UI/debug tooling can display it without another migration.
    pub owner_peer_id: String,
    /// Fresh direct and relay dialing information for the endpoint.
    pub owner_addr: iroh::EndpointAddr,
    /// Optional always-on workspace hosts. Guests dial these in addition to
    /// the inviter, so an accepted room does not become dependent on the
    /// inviter staying online. Absent from legacy v1 tokens.
    #[serde(default)]
    pub host_addrs: Vec<iroh::EndpointAddr>,
    /// Root-signed binding from the inviter's person identity to the Iroh
    /// endpoint/device key. Absent only on legacy invitations.
    #[serde(default)]
    pub owner_device_cert: Option<DeviceCert>,
    pub expiry_unix_seconds: u64,
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct InvitePayload<'a> {
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a iroh::EndpointAddr,
    host_addrs: &'a [iroh::EndpointAddr],
    expiry_unix_seconds: u64,
}

#[derive(Serialize)]
struct InvitePayloadV2<'a> {
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a iroh::EndpointAddr,
    host_addrs: &'a [iroh::EndpointAddr],
    owner_device_cert: &'a DeviceCert,
    expiry_unix_seconds: u64,
}

#[derive(Serialize)]
struct LegacyInvitePayload<'a> {
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a iroh::EndpointAddr,
    expiry_unix_seconds: u64,
}

#[derive(Serialize, Deserialize)]
struct Libp2pInviteToken {
    workspace_id: u64,
    room_id: String,
    owner_peer_id: String,
    owner_public_key: String,
    owner_addr: String,
    expiry_unix_seconds: u64,
    signature: Vec<u8>,
}

fn libp2p_payload(token: &Libp2pInviteToken) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        token.workspace_id,
        token.room_id,
        token.owner_peer_id,
        token.owner_public_key,
        token.owner_addr,
        token.expiry_unix_seconds
    )
}

fn legacy_libp2p_invite_error(json: &[u8]) -> Option<String> {
    use ed25519_dalek::Verifier as _;

    let token: Libp2pInviteToken = serde_json::from_slice(json).ok()?;
    if token.expiry_unix_seconds < now_unix_seconds() {
        return Some("legacy libp2p invite expired".into());
    }
    RoomId::parse(&token.room_id).filter(|room| !room.is_nil())?;
    let protobuf = B64.decode(token.owner_public_key.as_bytes()).ok()?;
    if protobuf.len() != 36 || protobuf[..4] != [0x08, 0x01, 0x12, 0x20] {
        return None;
    }
    let public: [u8; 32] = protobuf[4..].try_into().ok()?;
    let endpoint = crate::network::behaviour::endpoint_from_legacy_peer_id(&token.owner_peer_id)?;
    if endpoint.as_bytes() != &public {
        return None;
    }
    let signature: [u8; 64] = token.signature.as_slice().try_into().ok()?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&public).ok()?;
    key.verify(
        libp2p_payload(&token).as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&signature),
    )
    .ok()?;
    Some(
        "this is a valid legacy libp2p invite, but its TCP address cannot be dialed by the Iroh transport; ask the inviter for a fresh invite"
            .into(),
    )
}

fn signing_payload(token: &PeerInviteToken) -> Result<Vec<u8>, postcard::Error> {
    if let Some(cert) = &token.owner_device_cert {
        return postcard::to_allocvec(&InvitePayloadV2 {
            workspace_id: token.workspace_id,
            room_id: &token.room_id,
            owner_peer_id: &token.owner_peer_id,
            owner_addr: &token.owner_addr,
            host_addrs: &token.host_addrs,
            owner_device_cert: cert,
            expiry_unix_seconds: token.expiry_unix_seconds,
        });
    }
    postcard::to_allocvec(&InvitePayload {
        workspace_id: token.workspace_id,
        room_id: &token.room_id,
        owner_peer_id: &token.owner_peer_id,
        owner_addr: &token.owner_addr,
        host_addrs: &token.host_addrs,
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
    make_invite_token_with_hosts(key, workspace, owner_addr, &[], expiry_seconds_from_now)
}

/// Mint an invite carrying optional always-on workspace hosts.
pub fn make_invite_token_with_hosts(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_identity(
        key,
        workspace,
        owner_addr,
        host_addrs,
        None,
        expiry_seconds_from_now,
    )
}

pub fn make_invite_token_with_identity(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
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
    if let Some(cert) = &owner_device_cert {
        qos_user_identity::verify_device_cert(cert).map_err(std::io::Error::other)?;
        if cert.device_public_key != *key.public().as_bytes() {
            return Err(std::io::Error::other(
                "invite certificate does not belong to the signing device",
            ));
        }
    }
    let mut token = PeerInviteToken {
        workspace_id: workspace.id.raw(),
        room_id: workspace.room.to_string(),
        owner_peer_id: key.public().to_string(),
        owner_addr: owner_addr.clone(),
        host_addrs: host_addrs.to_vec(),
        owner_device_cert,
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
    let token: PeerInviteToken = match serde_json::from_slice(&json) {
        Ok(token) => token,
        Err(error) => {
            if let Some(message) = legacy_libp2p_invite_error(&json) {
                return Err(message);
            }
            return Err(format!("json parse: {error}"));
        }
    };
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
    if let Some(cert) = &token.owner_device_cert {
        qos_user_identity::verify_device_cert(cert)
            .map_err(|error| format!("identity -> device certificate invalid: {error}"))?;
        if cert.device_public_key != *endpoint.as_bytes() {
            return Err("device certificate does not name the inviting endpoint".into());
        }
    }
    let signature: [u8; 64] = token
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| "signature has the wrong length".to_string())?;
    let signature = iroh_base::Signature::from_bytes(&signature);
    let current = signing_payload(&token).map_err(|e| format!("canonical encode: {e}"))?;
    if endpoint.verify(&current, &signature).is_err() {
        if !token.host_addrs.is_empty() {
            return Err("signature mismatch".into());
        }
        let legacy = postcard::to_allocvec(&LegacyInvitePayload {
            workspace_id: token.workspace_id,
            room_id: &token.room_id,
            owner_peer_id: &token.owner_peer_id,
            owner_addr: &token.owner_addr,
            expiry_unix_seconds: token.expiry_unix_seconds,
        })
        .map_err(|e| format!("legacy canonical encode: {e}"))?;
        endpoint
            .verify(&legacy, &signature)
            .map_err(|_| "signature mismatch".to_string())?;
    }
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
    let person = token.owner_device_cert.as_ref().map(|cert| {
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => handle.clone(),
        }
    });
    tree.admit_collaborator(
        local,
        Collaborator::new(person.unwrap_or_else(|| handle.clone()), handle).with_session(session),
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
    fn round_trip_carries_signed_always_on_hosts() {
        let owner = iroh::SecretKey::from_bytes(&[13; 32]);
        let host = iroh::SecretKey::from_bytes(&[14; 32]);
        let host_addr = endpoint(&host, 4321);
        let encoded = make_invite_token_with_hosts(
            &owner,
            &workspace(5),
            &endpoint(&owner, 1234),
            std::slice::from_ref(&host_addr),
            3600,
        )
        .unwrap();
        let token = validate_invite_token(&encoded).unwrap();
        assert_eq!(token.host_addrs, vec![host_addr]);
    }

    #[test]
    fn identity_certificate_binds_inviter_person_to_device_endpoint() {
        use qos_user_identity::{DeviceCapabilities, sign_device_cert};

        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let device = iroh::SecretKey::from_bytes(&[42; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("cert");
        let ws = workspace(17);
        let encoded = make_invite_token_with_identity(
            &device,
            &ws,
            &endpoint(&device, 1717),
            &[],
            Some(cert.clone()),
            3600,
        )
        .expect("invite");
        let decoded = validate_invite_token(&encoded).expect("valid");
        assert_eq!(decoded.owner_device_cert, Some(cert));

        let mut tree = WorkspaceTree::default();
        let local = WorkspaceId(99);
        tree.workspaces.push(workspace(99));
        accept_invite_into(&mut tree, local, &encoded).expect("accept");
        let person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => unreachable!(),
        };
        assert!(
            tree.workspaces[0]
                .collaborators
                .iter()
                .any(|member| member.id == person)
        );
    }

    #[test]
    fn legacy_token_without_hosts_still_validates() {
        let key = iroh::SecretKey::from_bytes(&[15; 32]);
        let workspace = workspace(6);
        let addr = endpoint(&key, 2468);
        let mut token = PeerInviteToken {
            workspace_id: workspace.id.raw(),
            room_id: workspace.room.to_string(),
            owner_peer_id: key.public().to_string(),
            owner_addr: addr,
            host_addrs: Vec::new(),
            owner_device_cert: None,
            expiry_unix_seconds: now_unix_seconds() + 3600,
            signature: Vec::new(),
        };
        let legacy = postcard::to_allocvec(&LegacyInvitePayload {
            workspace_id: token.workspace_id,
            room_id: &token.room_id,
            owner_peer_id: &token.owner_peer_id,
            owner_addr: &token.owner_addr,
            expiry_unix_seconds: token.expiry_unix_seconds,
        })
        .unwrap();
        token.signature = key.sign(&legacy).to_bytes().to_vec();
        let encoded = B64.encode(serde_json::to_vec(&token).unwrap());
        assert!(validate_invite_token(&encoded).is_ok());
    }

    #[test]
    fn valid_libp2p_invite_gets_an_actionable_migration_error() {
        let key = iroh::SecretKey::from_bytes(&[16; 32]);
        let public = key.public();
        let mut peer_bytes = vec![0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
        peer_bytes.extend_from_slice(public.as_bytes());
        let owner_peer_id = bs58::encode(peer_bytes).into_string();
        let mut public_protobuf = vec![0x08, 0x01, 0x12, 0x20];
        public_protobuf.extend_from_slice(public.as_bytes());
        let mut token = Libp2pInviteToken {
            workspace_id: 7,
            room_id: workspace(7).room.to_string(),
            owner_peer_id,
            owner_public_key: B64.encode(public_protobuf),
            owner_addr: "/ip4/127.0.0.1/tcp/1234".into(),
            expiry_unix_seconds: now_unix_seconds() + 3600,
            signature: Vec::new(),
        };
        token.signature = key
            .sign(libp2p_payload(&token).as_bytes())
            .to_bytes()
            .to_vec();
        let encoded = B64.encode(serde_json::to_vec(&token).unwrap());
        let error = validate_invite_token(&encoded).unwrap_err();
        assert!(error.contains("legacy libp2p invite"));
        assert!(error.contains("fresh invite"));
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
