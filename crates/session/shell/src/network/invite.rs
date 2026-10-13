//! Signed invitation tickets for iroh workspace collaboration.
//!
//! An invitation binds the durable room identity to the inviter's stable iroh
//! endpoint id and its current dialing hints. The endpoint id is itself the
//! Ed25519 verification key, so no transport-specific PeerId/public-key pair is
//! duplicated in the token.

pub(crate) mod enrollment_driver;
#[cfg(test)]
#[path = "invite/native_exchange_tests.rs"]
mod exchange_tests;
pub(crate) use tilo_joined_room::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use crate::shell::state::{
    Collaborator, PeerOrigin, RoomId, Workspace, WorkspaceId, WorkspaceTree,
};

/// Authority carried by an invitation.  Owner is deliberately absent: a
/// bearer invite may add a collaborator, never transfer workspace ownership.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum InviteRole {
    Viewer,
    Commenter,
    #[default]
    Editor,
}

impl InviteRole {
    /// The room role an accepted invitation admits its holder with.
    #[must_use]
    pub fn role(self) -> crate::shell::state::CapRole {
        match self {
            Self::Viewer => crate::shell::state::CapRole::Viewer,
            Self::Commenter => crate::shell::state::CapRole::Commenter,
            Self::Editor => crate::shell::state::CapRole::Editor,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Commenter => "Commenter",
            Self::Editor => "Editor",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Viewer => Self::Commenter,
            Self::Commenter => Self::Editor,
            Self::Editor => Self::Viewer,
        }
    }
}

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
    /// inviter staying online.
    pub host_addrs: Vec<iroh::EndpointAddr>,
    /// Enrollment-bound identity for a home host. Signed with the invite so a
    /// recipient never treats an arbitrary same-root relay as its Files peer.
    pub home_host_pins: Vec<HomeHostPin>,
    /// Root-signed binding from the inviter's person identity to the Iroh
    /// endpoint/device key.
    pub owner_device_cert: Option<DeviceCert>,
    /// Maximum workspace role granted when this invite is redeemed.
    pub role: InviteRole,
    /// Random per-invite nonce. Names stay authority-side, but this makes
    /// two invitations minted in the same second distinct credentials so
    /// cancellation and redemption can target the exact invitation.
    pub invite_nonce: [u8; 16],
    /// Optional recipient person identity. When present, possession of the
    /// bearer is insufficient: redemption must present a device certificate
    /// chaining to this exact user key. This is the preferred shape for
    /// invitations that an always-on host may admit while the owner is away.
    #[serde(default)]
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// Owner-chosen display name covered by the signature. Hosts never accept
    /// a self-asserted name from the joining peer.
    #[serde(default)]
    pub invitee_name: Option<String>,
    pub expiry_unix_seconds: u64,
    pub signature: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct HomeHostPin {
    pub addr: iroh::EndpointAddr,
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

const MAX_HOME_HOST_PINS: usize = 16;

/// The signed bytes of an invitation: every field of the token except the
/// signature itself.
#[derive(Serialize)]
struct InvitePayload<'a> {
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a iroh::EndpointAddr,
    host_addrs: &'a [iroh::EndpointAddr],
    home_host_pins: &'a [HomeHostPin],
    owner_device_cert: Option<&'a DeviceCert>,
    role: InviteRole,
    invite_nonce: [u8; 16],
    recipient_user_public_key: Option<&'a [u8; 32]>,
    invitee_name: Option<&'a str>,
    expiry_unix_seconds: u64,
}

fn signing_payload(token: &PeerInviteToken) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(&InvitePayload {
        workspace_id: token.workspace_id,
        room_id: &token.room_id,
        owner_peer_id: &token.owner_peer_id,
        owner_addr: &token.owner_addr,
        host_addrs: &token.host_addrs,
        home_host_pins: &token.home_host_pins,
        owner_device_cert: token.owner_device_cert.as_ref(),
        role: token.role,
        invite_nonce: token.invite_nonce,
        recipient_user_public_key: token.recipient_user_public_key.as_ref(),
        invitee_name: token.invitee_name.as_deref(),
        expiry_unix_seconds: token.expiry_unix_seconds,
    })
}

/// Stable, non-secret identifier for one exact signed invite. Authorities use
/// it to correlate a redemption with the role they minted without putting a
/// room-wide "next arrival" grant on the wire.
#[must_use]
pub fn invite_id(token: &str) -> [u8; 32] {
    *blake3::hash(token.as_bytes()).as_bytes()
}

/// Mint an invite for `workspace` with no always-on hosts and no bound
/// signing identity.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, or if encoding the signed token fails.
pub fn make_invite_token(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_hosts(key, workspace, owner_addr, &[], expiry_seconds_from_now)
}

/// Mint an invite carrying optional always-on workspace hosts.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, or if encoding the signed token fails.
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

/// Mint an invite, optionally binding the inviter's identity-signed device
/// certificate so the guest can verify who is inviting them, not just which
/// device.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, if `owner_device_cert` is present but fails
/// verification or doesn't name `key`'s device, or if encoding the signed
/// token fails.
pub fn make_invite_token_with_identity(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_identity_and_role(
        key,
        workspace,
        owner_addr,
        host_addrs,
        owner_device_cert,
        InviteRole::Editor,
        expiry_seconds_from_now,
    )
}

/// Mint an identity-bound invite whose signed payload limits the role granted
/// on redemption.
///
/// # Errors
///
/// Returns an error when the workspace or endpoint is invalid, the optional
/// device certificate does not match the signing key, or the token cannot be
/// encoded.
pub fn make_invite_token_with_identity_and_role(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
    role: InviteRole,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_recipient(
        key,
        workspace,
        owner_addr,
        host_addrs,
        &[],
        owner_device_cert,
        InviteTerms {
            role,
            recipient: None,
            expiry_seconds_from_now,
        },
    )
}

/// What a signed invite grants: the role, the person it is bound to (if
/// anyone), and how long it stays redeemable.
#[derive(Debug, Clone, Copy)]
pub struct InviteTerms<'a> {
    pub role: InviteRole,
    pub recipient: Option<InviteRecipient<'a>>,
    pub expiry_seconds_from_now: u64,
}

/// The person identity an invite is bound to, and the display name the
/// inviter signs for them.
#[derive(Debug, Clone, Copy)]
pub struct InviteRecipient<'a> {
    pub user_public_key: [u8; 32],
    pub name: Option<&'a str>,
}

/// Mint an invite bound to one recipient person identity. The optional name is
/// signed by the inviter and therefore safe for an offline host to project.
///
/// # Errors
///
/// Returns an error if the workspace, endpoint, device certificate, or
/// serialized signed token is invalid.
pub fn make_invite_token_with_recipient(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    home_host_pins: &[HomeHostPin],
    owner_device_cert: Option<DeviceCert>,
    terms: InviteTerms<'_>,
) -> std::io::Result<String> {
    let InviteTerms {
        role,
        recipient,
        expiry_seconds_from_now,
    } = terms;
    let recipient_user_public_key = recipient.map(|recipient| recipient.user_public_key);
    let invitee_name = recipient.and_then(|recipient| recipient.name);
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
        tilo_user_identity::verify_device_cert(cert).map_err(std::io::Error::other)?;
        if cert.device_public_key != *key.public().as_bytes() {
            return Err(std::io::Error::other(
                "invite certificate does not belong to the signing device",
            ));
        }
    }
    let invitee_name = invitee_name.map(str::trim).filter(|name| !name.is_empty());
    if invitee_name.is_some_and(|name| name.len() > 128 || name.chars().any(char::is_control)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invitee name is empty, over 128 bytes, or contains control characters",
        ));
    }
    let mut token = PeerInviteToken {
        workspace_id: workspace.id.raw(),
        room_id: workspace.room.to_string(),
        owner_peer_id: key.public().to_string(),
        owner_addr: owner_addr.clone(),
        host_addrs: host_addrs.to_vec(),
        home_host_pins: home_host_pins.to_vec(),
        owner_device_cert,
        role,
        invite_nonce: *uuid::Uuid::new_v4().as_bytes(),
        recipient_user_public_key,
        invitee_name: invitee_name.map(str::to_string),
        expiry_unix_seconds: tilo_clock::unix_now().saturating_add(expiry_seconds_from_now),
        signature: Vec::new(),
    };
    token.signature = key
        .sign(&signing_payload(&token).map_err(std::io::Error::other)?)
        .to_bytes()
        .to_vec();
    let json = serde_json::to_vec(&token).map_err(std::io::Error::other)?;
    Ok(B64.encode(json))
}

/// Decode and verify a base64 invite token: signature, expiry, room id, and
/// (if present) the bound device certificate.
///
/// # Errors
///
/// Returns an error describing which check failed: base64/JSON decoding,
/// expiry, an invalid or unminted room id, a device-certificate/endpoint
/// mismatch, or a signature that does not cover the token.
pub fn validate_invite_token(token_str: &str) -> Result<PeerInviteToken, String> {
    let json = B64
        .decode(token_str)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let token: PeerInviteToken =
        serde_json::from_slice(&json).map_err(|error| format!("json parse: {error}"))?;
    if token.expiry_unix_seconds < tilo_clock::unix_now() {
        return Err("token expired".into());
    }
    if token.invitee_name.as_ref().is_some_and(|name| {
        name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
    }) {
        return Err("invite carries an invalid recipient name".into());
    }
    if token.invitee_name.is_some() && token.recipient_user_public_key.is_none() {
        return Err("invite carries a name without binding a recipient identity".into());
    }
    if token.home_host_pins.len() > MAX_HOME_HOST_PINS {
        return Err("invite carries too many home-host pins".into());
    }
    let pinned_owner = if token.home_host_pins.is_empty() {
        None
    } else {
        Some(
            token
                .owner_device_cert
                .as_ref()
                .ok_or("invite carries home-host pins without an owner certificate")?,
        )
    };
    let mut pinned_endpoints = std::collections::HashSet::new();
    for pin in &token.home_host_pins {
        ed25519_dalek::VerifyingKey::from_bytes(&pin.user_public_key)
            .map_err(|_| "invite carries an invalid home-host user key")?;
        if pin.device_public_key.iter().all(|byte| *byte == 0) {
            return Err("invite carries an empty home-host device key".into());
        }
        ed25519_dalek::VerifyingKey::from_bytes(&pin.device_public_key)
            .map_err(|_| "invite carries an invalid home-host device key")?;
        if pinned_owner.is_some_and(|owner| owner.user_public_key != pin.user_public_key) {
            return Err("invite home-host pin belongs to a different user".into());
        }
        if !pinned_endpoints.insert(*pin.addr.id.as_bytes()) {
            return Err("invite carries duplicate home-host pins".into());
        }
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
        tilo_user_identity::verify_device_cert(cert)
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
    let payload = signing_payload(&token).map_err(|e| format!("canonical encode: {e}"))?;
    endpoint
        .verify(&payload, &signature)
        .map_err(|_| "signature verification failed".to_string())?;
    Ok(token)
}

/// Validate `token_str` and bind `local`'s workspace to the room it names.
///
/// # Errors
///
/// Returns an error if the token fails [`validate_invite_token`], if it
/// carries no valid room id, or if `local` names no local workspace to
/// accept the invite into.
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
        Workspace::new(WorkspaceId(id), format!("ws-{id}"))
    }

    fn endpoint(key: &iroh::SecretKey, port: u16) -> iroh::EndpointAddr {
        iroh::EndpointAddr::new(key.public())
            .with_ip_addr(format!("127.0.0.1:{port}").parse().unwrap())
    }

    #[test]
    fn invitations_round_trip_their_room_hosts_and_roster() {
        // Phase `round_trip_binds_room_key_and_dialing_address`.
        {
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
        // Phase `round_trip_carries_signed_always_on_hosts`.
        {
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
        // Phase `invitations_minted_in_the_same_second_remain_distinct_and_valid`.
        {
            let owner = iroh::SecretKey::from_bytes(&[32; 32]);
            let workspace = workspace(32);
            let addr = endpoint(&owner, 3232);
            let first = make_invite_token_with_identity_and_role(
                &owner,
                &workspace,
                &addr,
                &[],
                None,
                InviteRole::Editor,
                3600,
            )
            .expect("first invite");
            let second = make_invite_token_with_identity_and_role(
                &owner,
                &workspace,
                &addr,
                &[],
                None,
                InviteRole::Editor,
                3600,
            )
            .expect("second invite");
            assert_ne!(first, second, "each invitation is independently revocable");
            assert_ne!(invite_id(&first), invite_id(&second));
            validate_invite_token(&first).expect("first remains valid");
            validate_invite_token(&second).expect("second remains valid");
        }
        // Phase `accepting_invite_adopts_room_and_roster_identity`.
        {
            let key = iroh::SecretKey::from_bytes(&[11; 32]);
            let host = workspace(11);
            let encoded = make_invite_token(&key, &host, &endpoint(&key, 3), 3600).unwrap();
            let mut guest = WorkspaceTree::default();
            let guest_id = WorkspaceId(4242);
            guest
                .desktop
                .workspaces
                .push(Workspace::new(guest_id, "guest"));
            assert_eq!(
                accept_invite_into(&mut guest, guest_id, &encoded).unwrap(),
                host.room
            );
            assert_eq!(guest.room_of(guest_id), Some(host.room));
            assert_eq!(guest.desktop.workspaces[0].collaborators.len(), 1);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn every_signed_invitation_field_is_tamper_evident() {
        // Phase `home_host_pin_is_signed_and_tamper_evident`.
        {
            use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

            let owner = iroh::SecretKey::from_bytes(&[0x21; 32]);
            let relay = iroh::SecretKey::from_bytes(&[0x22; 32]);
            let other_relay = iroh::SecretKey::from_bytes(&[0x23; 32]);
            let user = ed25519_dalek::SigningKey::from_bytes(&[0x24; 32]);
            let host_device = iroh::SecretKey::from_bytes(&[0x25; 32]);
            let mut owner_cert = DeviceCert {
                user_public_key: user.verifying_key().to_bytes(),
                device_public_key: *owner.public().as_bytes(),
                device_label: "Owner laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut owner_cert, &user).expect("owner certificate");
            let pin = HomeHostPin {
                addr: endpoint(&relay, 4321),
                user_public_key: user.verifying_key().to_bytes(),
                device_public_key: *host_device.public().as_bytes(),
            };
            let encoded = make_invite_token_with_recipient(
                &owner,
                &workspace(25),
                &endpoint(&owner, 2525),
                std::slice::from_ref(&pin.addr),
                std::slice::from_ref(&pin),
                Some(owner_cert),
                InviteTerms {
                    role: InviteRole::Editor,
                    recipient: None,
                    expiry_seconds_from_now: 3600,
                },
            )
            .expect("pinned invite");
            let decoded = validate_invite_token(&encoded).expect("valid pinned invite");
            assert_eq!(decoded.home_host_pins, vec![pin]);

            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.home_host_pins[0].addr = endpoint(&other_relay, 4343);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(
                validate_invite_token(&forged)
                    .expect_err("a changed host pin must invalidate the signature")
                    .contains("signature")
            );
        }
        // Phase `selected_role_is_signed_and_tamper_evident`.
        {
            let owner = iroh::SecretKey::from_bytes(&[31; 32]);
            let encoded = make_invite_token_with_identity_and_role(
                &owner,
                &workspace(31),
                &endpoint(&owner, 3131),
                &[],
                None,
                InviteRole::Viewer,
                3600,
            )
            .expect("viewer invite");
            assert_eq!(
                validate_invite_token(&encoded).expect("valid").role,
                InviteRole::Viewer
            );

            let bytes = B64.decode(encoded).expect("base64");
            let mut token: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            token.role = InviteRole::Editor;
            let forged = B64.encode(serde_json::to_vec(&token).expect("json"));
            assert!(
                validate_invite_token(&forged)
                    .expect_err("role widening must invalidate the signature")
                    .contains("signature")
            );
        }
        // Phase `recipient_binding_and_name_are_signature_covered`.
        {
            let owner = iroh::SecretKey::from_bytes(&[18; 32]);
            let workspace = workspace(18);
            let recipient = [0x44; 32];
            let encoded = make_invite_token_with_recipient(
                &owner,
                &workspace,
                &endpoint(&owner, 1818),
                &[],
                &[],
                None,
                InviteTerms {
                    role: InviteRole::Commenter,
                    recipient: Some(InviteRecipient {
                        user_public_key: recipient,
                        name: Some("Ada"),
                    }),
                    expiry_seconds_from_now: 3600,
                },
            )
            .expect("bound invite");
            let decoded = validate_invite_token(&encoded).expect("valid bound invite");
            assert_eq!(decoded.recipient_user_public_key, Some(recipient));
            assert_eq!(decoded.invitee_name.as_deref(), Some("Ada"));

            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.recipient_user_public_key = Some([0x55; 32]);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(validate_invite_token(&forged).is_err());
        }
        // Phase `an_unsigned_recipient_cannot_be_grafted_onto_a_bearer`.
        {
            let owner = iroh::SecretKey::from_bytes(&[19; 32]);
            let workspace = workspace(19);
            let encoded = make_invite_token(&owner, &workspace, &endpoint(&owner, 1919), 3600)
                .expect("bearer invite");
            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.recipient_user_public_key = Some([0x66; 32]);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(validate_invite_token(&forged).is_err());
        }
        // Phase `tampering_any_signed_route_field_is_rejected`.
        {
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
        // Phase `substituted_endpoint_is_rejected`.
        {
            let owner = iroh::SecretKey::from_bytes(&[9; 32]);
            let attacker = iroh::SecretKey::from_bytes(&[10; 32]);
            let encoded =
                make_invite_token(&owner, &workspace(1), &endpoint(&owner, 1), 3600).unwrap();
            let bytes = B64.decode(encoded).unwrap();
            let mut token: PeerInviteToken = serde_json::from_slice(&bytes).unwrap();
            token.owner_addr = endpoint(&attacker, 2);
            let forged = B64.encode(serde_json::to_vec(&token).unwrap());
            assert!(validate_invite_token(&forged).is_err());
        }
    }

    #[test]
    fn invitation_identity_is_bound_and_unsupported_invites_are_refused() {
        // Phase `identity_certificate_binds_inviter_person_to_device_endpoint`.
        {
            use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

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
            tree.desktop.workspaces.push(workspace(99));
            accept_invite_into(&mut tree, local, &encoded).expect("accept");
            let collab_types::caps::Principal::Person(person) =
                collab_types::caps::Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(
                        root.verifying_key().to_bytes(),
                    ),
                )
            else {
                unreachable!()
            };
            assert!(
                tree.desktop.workspaces[0]
                    .collaborators
                    .iter()
                    .any(|member| member.id == person)
            );
        }
        // Phase `nil_room_cannot_be_invited`.
        {
            let key = iroh::SecretKey::from_bytes(&[12; 32]);
            let mut ws = workspace(4);
            ws.set_room(RoomId::nil());
            assert!(make_invite_token(&key, &ws, &endpoint(&key, 4), 3600).is_err());
        }
    }
}
