//! Signed room invitations.
//!
//! An invitation binds a durable room identity to the inviting device's
//! stable Iroh endpoint id and its current dialing hints. The endpoint id is
//! itself the Ed25519 verification key, so no transport-specific key pair is
//! duplicated in the token. Everything below the signature is covered by it:
//! changing the role, the room, a host, the recipient or the standing flag
//! invalidates the token.
//!
//! Two shapes share the one token:
//!
//! - A **single-use** invitation, the default. The room authority consumes it
//!   on first redemption, and it may name the one person allowed to redeem it.
//! - A **standing** invitation to a public room. Any certified device may
//!   redeem it, any number of times, until it expires or the room authority
//!   revokes it. Because it is a bearer credential handed to strangers it is
//!   attenuated at the source: it can grant at most [`InviteRole::Commenter`],
//!   and it names no recipient. Redemption still binds the redeeming device's
//!   own certificate, so a standing invitation admits people, not tokens.
//!
//! This module is transport-neutral so the shell (which mints and redeems
//! invitations for rooms it governs) and a workspace host (which redeems
//! standing invitations it was delegated) validate exactly the same bytes.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use collab_types::caps::Verb;
use iroh_base::{EndpointAddr, EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// The most always-on hosts an invitation may pin.
pub const MAX_HOME_HOST_PINS: usize = 16;
/// The longest signed invitee name, in bytes.
pub const MAX_INVITEE_NAME_BYTES: usize = 128;

/// Authority carried by an invitation. Owner is deliberately absent: an
/// invitation may add a collaborator, never transfer room ownership.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Eq, Clone, Copy, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InviteRole {
    Viewer,
    Commenter,
    #[default]
    Editor,
}

impl InviteRole {
    /// The capability verb the role becomes on redemption.
    #[must_use]
    pub fn verb(self) -> Verb {
        match self {
            Self::Viewer => Verb::Read,
            Self::Commenter => Verb::Comment,
            Self::Editor => Verb::Write,
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

    /// The highest role a standing invitation may carry.
    pub const STANDING_CEILING: Self = Self::Commenter;

    /// Whether `self` grants no more than `ceiling`.
    #[must_use]
    pub fn within(self, ceiling: Self) -> bool {
        ceiling.verb().implies(self.verb())
    }
}

/// An always-on host pinned by the enrollment that made it this person's
/// home peer, so a recipient never treats an arbitrary same-root relay as its
/// Files peer.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct HomeHostPin {
    pub addr: EndpointAddr,
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct PeerInviteToken {
    /// Inviter-local routing handle. The durable shared identity is `room_id`.
    pub workspace_id: u64,
    pub room_id: String,
    /// z-base-32 Iroh `EndpointId` of the signing device.
    pub owner_peer_id: String,
    /// Fresh direct and relay dialing information for the endpoint.
    pub owner_addr: EndpointAddr,
    /// Always-on workspace hosts. Guests dial these in addition to the
    /// inviter, so an accepted room does not depend on the inviter staying
    /// online.
    pub host_addrs: Vec<EndpointAddr>,
    /// Enrollment-bound identity for a home host.
    pub home_host_pins: Vec<HomeHostPin>,
    /// Root-signed binding from the inviter's person identity to the Iroh
    /// endpoint/device key.
    pub owner_device_cert: Option<DeviceCert>,
    /// Maximum room role granted when this invitation is redeemed.
    pub role: InviteRole,
    /// Random per-invitation nonce, so two invitations minted in the same
    /// second are distinct credentials that can be revoked separately.
    pub invite_nonce: [u8; 16],
    /// Optional recipient person identity. When present, possession of the
    /// token is insufficient: redemption must present a device certificate
    /// chaining to this exact user key.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// Owner-chosen display name covered by the signature. Hosts never accept
    /// a self-asserted name from the joining peer.
    pub invitee_name: Option<String>,
    /// Multi-use invitation to a public room; see the module docs.
    pub standing: bool,
    pub expiry_unix_seconds: u64,
    pub signature: Vec<u8>,
}

/// Everything an invitation says, before it is signed.
#[derive(Debug, Clone)]
pub struct InviteDraft {
    pub workspace_id: u64,
    /// The room, as its canonical uuid string. Must not be the nil uuid.
    pub room_id: String,
    pub owner_addr: EndpointAddr,
    pub host_addrs: Vec<EndpointAddr>,
    pub home_host_pins: Vec<HomeHostPin>,
    pub owner_device_cert: Option<DeviceCert>,
    pub role: InviteRole,
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub standing: bool,
    pub expiry_unix_seconds: u64,
}

impl InviteDraft {
    /// A single-use invitation carrying `role`, with nothing else pinned.
    #[must_use]
    pub fn new(
        workspace_id: u64,
        room_id: impl Into<String>,
        owner_addr: EndpointAddr,
        role: InviteRole,
        expiry_unix_seconds: u64,
    ) -> Self {
        Self {
            workspace_id,
            room_id: room_id.into(),
            owner_addr,
            host_addrs: Vec::new(),
            home_host_pins: Vec::new(),
            owner_device_cert: None,
            role,
            recipient_user_public_key: None,
            invitee_name: None,
            standing: false,
            expiry_unix_seconds,
        }
    }
}

#[derive(Serialize)]
struct InvitePayload<'a> {
    tag: &'static str,
    workspace_id: u64,
    room_id: &'a str,
    owner_peer_id: &'a str,
    owner_addr: &'a EndpointAddr,
    host_addrs: &'a [EndpointAddr],
    home_host_pins: &'a [HomeHostPin],
    owner_device_cert: Option<&'a DeviceCert>,
    role: InviteRole,
    invite_nonce: [u8; 16],
    recipient_user_public_key: Option<&'a [u8; 32]>,
    invitee_name: Option<&'a str>,
    standing: bool,
    expiry_unix_seconds: u64,
}

/// Domain tag mixed into every invitation's signed bytes.
const INVITE_SIGNING_TAG: &str = "tilo-room-invite";

fn signing_payload(token: &PeerInviteToken) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(&InvitePayload {
        tag: INVITE_SIGNING_TAG,
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
        standing: token.standing,
        expiry_unix_seconds: token.expiry_unix_seconds,
    })
}

/// Stable, non-secret identifier for one exact signed invitation.
/// Authorities use it to correlate a redemption with the grant they minted
/// without putting a room-wide "next arrival" grant on the wire.
#[must_use]
pub fn invite_id(token: &str) -> [u8; 32] {
    *blake3::hash(token.trim().as_bytes()).as_bytes()
}

/// Shape rules shared by minting and validation, so a token that could not
/// be minted cannot be accepted either.
fn check_shape(token: &PeerInviteToken) -> Result<(), String> {
    match uuid::Uuid::parse_str(&token.room_id) {
        Ok(room) if !room.is_nil() => {}
        Ok(_) => return Err("invite names the unminted room".into()),
        Err(_) => return Err("invite carries no valid room id".into()),
    }
    if token.invitee_name.as_ref().is_some_and(|name| {
        name.is_empty() || name.len() > MAX_INVITEE_NAME_BYTES || name.chars().any(char::is_control)
    }) {
        return Err("invite carries an invalid recipient name".into());
    }
    if token.invitee_name.is_some() && token.recipient_user_public_key.is_none() {
        return Err("invite carries a name without binding a recipient identity".into());
    }
    if token.standing {
        if !token.role.within(InviteRole::STANDING_CEILING) {
            return Err(format!(
                "a standing invite grants at most {}, not {}",
                InviteRole::STANDING_CEILING.label(),
                token.role.label()
            ));
        }
        if token.recipient_user_public_key.is_some() || token.invitee_name.is_some() {
            return Err("a standing invite is for anyone, so it names no recipient".into());
        }
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
    let endpoint: EndpointId = token
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
    Ok(())
}

/// Sign `draft` with the inviting device's key and encode it as the bearer
/// string people paste.
///
/// # Errors
///
/// A description of the first rule the draft breaks: a nil room, an address
/// or certificate that does not belong to `key`, a malformed name, a standing
/// invitation that names a recipient or grants more than Commenter, or an
/// encoding failure.
pub fn mint_invite(key: &SecretKey, draft: InviteDraft) -> Result<String, String> {
    if draft.owner_addr.id != key.public() {
        return Err("invite address does not belong to the signing device".into());
    }
    let invitee_name = draft
        .invitee_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let mut token = PeerInviteToken {
        workspace_id: draft.workspace_id,
        room_id: draft.room_id,
        owner_peer_id: key.public().to_string(),
        owner_addr: draft.owner_addr,
        host_addrs: draft.host_addrs,
        home_host_pins: draft.home_host_pins,
        owner_device_cert: draft.owner_device_cert,
        role: draft.role,
        invite_nonce: *uuid::Uuid::new_v4().as_bytes(),
        recipient_user_public_key: draft.recipient_user_public_key,
        invitee_name,
        standing: draft.standing,
        expiry_unix_seconds: draft.expiry_unix_seconds,
        signature: Vec::new(),
    };
    check_shape(&token)?;
    let payload = signing_payload(&token).map_err(|e| format!("canonical encode: {e}"))?;
    token.signature = key.sign(&payload).to_bytes().to_vec();
    let json = serde_json::to_vec(&token).map_err(|e| format!("json encode: {e}"))?;
    Ok(B64.encode(json))
}

/// Decode and verify a bearer invitation at `now_unix_seconds`: shape,
/// expiry, the bound device certificate, and the signature.
///
/// # Errors
///
/// A description of the first check that failed.
pub fn validate_invite_token_at(
    token_str: &str,
    now_unix_seconds: u64,
) -> Result<PeerInviteToken, String> {
    let json = B64
        .decode(token_str.trim())
        .map_err(|e| format!("base64 decode: {e}"))?;
    let token: PeerInviteToken =
        serde_json::from_slice(&json).map_err(|e| format!("json parse: {e}"))?;
    if token.expiry_unix_seconds < now_unix_seconds {
        return Err("token expired".into());
    }
    check_shape(&token)?;
    let endpoint: EndpointId = token
        .owner_peer_id
        .parse()
        .map_err(|e| format!("endpoint id parse: {e}"))?;
    let signature: [u8; 64] = token
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| "signature has the wrong length".to_string())?;
    let payload = signing_payload(&token).map_err(|e| format!("canonical encode: {e}"))?;
    endpoint
        .verify(&payload, &Signature::from_bytes(&signature))
        .map_err(|_| "signature verification failed".to_string())?;
    Ok(token)
}

/// [`validate_invite_token_at`] at this machine's clock.
///
/// # Errors
///
/// As [`validate_invite_token_at`].
pub fn validate_invite_token(token_str: &str) -> Result<PeerInviteToken, String> {
    validate_invite_token_at(token_str, now_unix_seconds())
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn addr(key: &SecretKey) -> EndpointAddr {
        EndpointAddr::new(key.public())
    }

    fn tamper(encoded: &str, edit: impl FnOnce(&mut PeerInviteToken)) -> String {
        let mut token: PeerInviteToken =
            serde_json::from_slice(&B64.decode(encoded).unwrap()).unwrap();
        edit(&mut token);
        B64.encode(serde_json::to_vec(&token).unwrap())
    }

    #[test]
    fn standing_invites_are_capped_at_commenter_and_name_nobody() {
        let key = SecretKey::from_bytes(&[3; 32]);
        let mut draft = InviteDraft::new(1, room(), addr(&key), InviteRole::Editor, u64::MAX);
        draft.standing = true;
        assert!(
            mint_invite(&key, draft.clone())
                .unwrap_err()
                .contains("at most Commenter")
        );
        draft.role = InviteRole::Commenter;
        draft.recipient_user_public_key = Some([4; 32]);
        assert!(
            mint_invite(&key, draft.clone())
                .unwrap_err()
                .contains("names no recipient")
        );
        draft.recipient_user_public_key = None;
        let viewer = InviteDraft {
            role: InviteRole::Viewer,
            ..draft.clone()
        };
        assert!(validate_invite_token(&mint_invite(&key, viewer).unwrap()).is_ok());
        let encoded = mint_invite(&key, draft).unwrap();
        let token = validate_invite_token(&encoded).unwrap();
        assert!(token.standing);
        assert_eq!(token.role, InviteRole::Commenter);
    }

    #[test]
    fn a_standing_invite_cannot_be_escalated_or_made_single_use_after_signing() {
        let key = SecretKey::from_bytes(&[5; 32]);
        let mut draft = InviteDraft::new(1, room(), addr(&key), InviteRole::Commenter, u64::MAX);
        draft.standing = true;
        let encoded = mint_invite(&key, draft).unwrap();

        let escalated = tamper(&encoded, |token| token.role = InviteRole::Editor);
        assert!(
            validate_invite_token(&escalated)
                .unwrap_err()
                .contains("at most Commenter"),
            "an escalated standing invite is refused by shape before the signature"
        );
        let re_signed_editor = {
            // Even a validly-signed widening fails: the ceiling is checked
            // on every validation, not trusted from the minting side.
            let mut token: PeerInviteToken =
                serde_json::from_slice(&B64.decode(&encoded).unwrap()).unwrap();
            token.role = InviteRole::Editor;
            token.signature = key
                .sign(&signing_payload(&token).unwrap())
                .to_bytes()
                .to_vec();
            B64.encode(serde_json::to_vec(&token).unwrap())
        };
        assert!(validate_invite_token(&re_signed_editor).is_err());
        let single_use = tamper(&encoded, |token| token.standing = false);
        assert!(
            validate_invite_token(&single_use)
                .unwrap_err()
                .contains("signature")
        );
    }

    #[test]
    fn a_single_use_invite_cannot_be_turned_standing() {
        let key = SecretKey::from_bytes(&[6; 32]);
        let encoded = mint_invite(
            &key,
            InviteDraft::new(1, room(), addr(&key), InviteRole::Viewer, u64::MAX),
        )
        .unwrap();
        let forged = tamper(&encoded, |token| token.standing = true);
        assert!(
            validate_invite_token(&forged)
                .unwrap_err()
                .contains("signature")
        );
    }

    #[test]
    fn expiry_is_optional_in_practice_and_enforced_when_set() {
        let key = SecretKey::from_bytes(&[7; 32]);
        let mut draft = InviteDraft::new(1, room(), addr(&key), InviteRole::Commenter, 100);
        draft.standing = true;
        let expiring = mint_invite(&key, draft.clone()).unwrap();
        assert!(validate_invite_token_at(&expiring, 99).is_ok());
        assert_eq!(
            validate_invite_token_at(&expiring, 101).unwrap_err(),
            "token expired"
        );
        draft.expiry_unix_seconds = u64::MAX;
        let open = mint_invite(&key, draft).unwrap();
        assert!(validate_invite_token_at(&open, u64::MAX - 1).is_ok());
    }

    #[test]
    fn invite_id_is_stable_under_surrounding_whitespace() {
        let key = SecretKey::from_bytes(&[8; 32]);
        let encoded = mint_invite(
            &key,
            InviteDraft::new(1, room(), addr(&key), InviteRole::Viewer, u64::MAX),
        )
        .unwrap();
        assert_eq!(invite_id(&encoded), invite_id(&format!(" {encoded}\n")));
        assert!(validate_invite_token(&format!("{encoded}\n")).is_ok());
    }
}
