//! Transport-neutral, signed workspace invitations and proof-of-possession admission.

use crate::DocumentRow;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

const INVITATION_VERSION: u32 = 1;
/// An invitation that also names who sent it.
const INVITATION_WITH_INVITER_VERSION: u32 = 2;
/// Longest inviter name an invitation carries, in characters.
pub const MAX_INVITER_NAME_CHARS: usize = 80;
const JOIN_VERSION: u32 = 1;
const INVITATION_DOMAIN: &[u8] = b"tilo/workspace-invitation/v1\0";
const JOIN_PROOF_DOMAIN: &[u8] = b"tilo/workspace-join-proof/v1\0";
const MEMBERSHIP_PROOF_DOMAIN: &[u8] = b"tilo/workspace-membership-proof/v1\0";
const TRANSPORT_PIN_DOMAIN: &[u8] = b"tilo/workspace-transport-pin/v1\0";
const DELEGATION_DOMAIN: &[u8] = b"tilo/workspace-delegation/v1\0";

pub const TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1: &str = "iroh-owner-endpoint-addr-v1";
pub const TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1: &str =
    "iroh-workspace-host-endpoint-addr-v1";
pub const TRANSPORT_TILO_SESSION_JOIN_HTTP_V1: &str = "tilo-session-join-http-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberRole {
    Viewer,
    Commenter,
    Editor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportHint {
    pub kind: String,
    pub opaque_json: String,
    /// Root-certified host identity whose exact device key must terminate the
    /// hinted transport. This pin is covered by the invitation signature.
    pub host: DeviceCert,
    /// Domain-separated service key actually terminating this endpoint.
    pub service_public_key: [u8; 32],
    /// Host-device signature binding the exact hint and service key.
    #[serde(with = "serde_bytes")]
    pub host_signature: [u8; 64],
}

#[derive(Serialize)]
struct TransportPinView<'a> {
    kind: &'a str,
    opaque_json: &'a str,
    service_public_key: &'a [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteGrant {
    pub workspace_id: u64,
    pub room: String,
    pub authority: DeviceCert,
    /// Device that signs this invitation. It is either `authority` or an
    /// explicitly delegated room-scoped workspace service.
    pub issuer: DeviceCert,
    pub delegation: Option<WorkspaceDelegation>,
    pub role: MemberRole,
    pub nonce: [u8; 16],
    /// `Some` is a recipient-bound invitation and must match exactly. `None`
    /// is an explicit single-use bearer invitation for an unknown person.
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
    pub transport: Vec<TransportHint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDelegationPermissions {
    pub issue_invitations: bool,
    pub admit_invited_members: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDelegation {
    pub version: u32,
    pub room: String,
    pub workspace_id: u64,
    pub authority_device_public_key: [u8; 32],
    pub delegate: DeviceCert,
    pub permissions: WorkspaceDelegationPermissions,
    pub nonce: [u8; 16],
    pub expires_at_ns: Option<u64>,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct DelegationView<'a> {
    version: u32,
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: &'a [u8; 32],
    delegate: &'a DeviceCert,
    permissions: WorkspaceDelegationPermissions,
    nonce: &'a [u8; 16],
    expires_at_ns: Option<u64>,
}

/// The person who sent an invitation, as the issuer recorded them under its
/// signature: the user root of the device that asked for it and the name
/// they gave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inviter {
    pub user_public_key: [u8; 32],
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitation {
    version: u32,
    grant: InviteGrant,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitationWithInviter {
    version: u32,
    grant: InviteGrant,
    inviter: Inviter,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Serialize)]
struct InvitationView<'a> {
    version: u32,
    grant: &'a InviteGrant,
}

#[derive(Serialize)]
struct InvitationWithInviterView<'a> {
    version: u32,
    grant: &'a InviteGrant,
    inviter: &'a Inviter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedJoin {
    invite_id: [u8; 32],
    grant: InviteGrant,
    inviter: Option<Inviter>,
    signed_invitation: Vec<u8>,
}

impl VerifiedJoin {
    pub fn invite_id(&self) -> [u8; 32] {
        self.invite_id
    }

    pub fn grant(&self) -> &InviteGrant {
        &self.grant
    }

    /// Who sent the invitation, when its issuer recorded them.
    pub fn inviter(&self) -> Option<&Inviter> {
        self.inviter.as_ref()
    }

    pub fn signed_invitation(&self) -> &[u8] {
        &self.signed_invitation
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallenge {
    pub version: u32,
    pub signed_invitation: Vec<u8>,
    pub room: String,
    pub workspace_id: u64,
    pub authority_device_public_key: [u8; 32],
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}

impl JoinChallenge {
    /// Issue a join challenge for a device redeeming `join`.
    ///
    /// # Errors
    ///
    /// Returns an error if the device certificate does not verify at `now_ns` or the
    /// challenge has already expired.
    pub fn new(
        join: &VerifiedJoin,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<Self, String> {
        validate_certificate(device_cert, &RevocationList::new(), now_ns)?;
        if join
            .grant
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != device_cert.user_public_key)
        {
            return Err("invitation recipient does not match device certificate".into());
        }
        if expires_at_ns <= now_ns || expires_at_ns > join.grant.expires_at_ns {
            return Err("join challenge has invalid expiry".into());
        }
        Ok(Self {
            version: JOIN_VERSION,
            signed_invitation: join.signed_invitation.clone(),
            room: join.grant.room.clone(),
            workspace_id: join.grant.workspace_id,
            authority_device_public_key: join.grant.authority.device_public_key,
            user_public_key: device_cert.user_public_key,
            device_public_key: device_cert.device_public_key,
            request_id,
            nonce,
            expires_at_ns,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinProof {
    pub challenge: JoinChallenge,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRedemption {
    join: VerifiedJoin,
    certificate: DeviceCert,
    request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallenge {
    pub version: u32,
    pub room: String,
    pub workspace_id: u64,
    pub authority_device_public_key: [u8; 32],
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}

impl MembershipChallenge {
    /// Issue a membership refresh challenge for a device of `room`.
    ///
    /// # Errors
    ///
    /// Returns an error if a certificate does not verify at `now_ns` or the challenge has
    /// already expired.
    pub fn new(
        room: &str,
        workspace_id: u64,
        authority: &DeviceCert,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now_ns: u64,
    ) -> Result<Self, String> {
        let room = uuid::Uuid::parse_str(room)
            .map_err(|_| "membership room is not a UUID".to_string())?
            .to_string();
        validate_certificate(authority, &RevocationList::new(), now_ns)?;
        validate_certificate(device_cert, &RevocationList::new(), now_ns)?;
        if expires_at_ns <= now_ns {
            return Err("membership challenge has invalid expiry".into());
        }
        Ok(Self {
            version: JOIN_VERSION,
            room,
            workspace_id,
            authority_device_public_key: authority.device_public_key,
            user_public_key: device_cert.user_public_key,
            device_public_key: device_cert.device_public_key,
            request_id,
            nonce,
            expires_at_ns,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipProof {
    pub challenge: MembershipChallenge,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMembership {
    challenge: MembershipChallenge,
    device_cert: DeviceCert,
}

impl VerifiedMembership {
    pub fn room(&self) -> &str {
        &self.challenge.room
    }
    pub fn workspace_id(&self) -> u64 {
        self.challenge.workspace_id
    }
    pub fn authority_device_public_key(&self) -> [u8; 32] {
        self.challenge.authority_device_public_key
    }
    pub fn request_id(&self) -> [u8; 32] {
        self.challenge.request_id
    }
    pub fn device_cert(&self) -> &DeviceCert {
        &self.device_cert
    }
}

impl VerifiedRedemption {
    pub fn join(&self) -> &VerifiedJoin {
        &self.join
    }

    pub fn certificate(&self) -> &DeviceCert {
        &self.certificate
    }

    pub fn request_id(&self) -> [u8; 32] {
        self.request_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinTransportRequest {
    Inspect { signed_invitation: Vec<u8> },
    Challenge(JoinChallengeRequest),
    Complete(JoinCompleteRequest),
    MembershipChallenge(MembershipChallengeRequest),
    MembershipComplete(MembershipCompleteRequest),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeRequest {
    pub signed_invitation: Vec<u8>,
    pub device_cert: DeviceCert,
    pub request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeResponse {
    pub challenge: JoinChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCompleteRequest {
    pub proof: JoinProof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeRequest {
    pub room: String,
    pub workspace_id: u64,
    pub device_cert: DeviceCert,
    pub request_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipChallengeResponse {
    pub challenge: MembershipChallenge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipCompleteRequest {
    pub proof: MembershipProof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinInspection {
    pub invite_id: [u8; 32],
    pub grant: InviteGrant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinBootstrap {
    pub version: u32,
    pub invite_id: [u8; 32],
    pub device_cert: DeviceCert,
    pub role: MemberRole,
    pub room: String,
    pub workspace_id: u64,
    pub authority: DeviceCert,
    pub certified_roster: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    pub records: Vec<DocumentRow>,
    pub governance_messages: Vec<Vec<u8>>,
    pub generation: u64,
}

/// Owner-specific bootstrap data. Authority is established only by admitting
/// the signed governance and deriving the structural Owner from that state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerBootstrapV1 {
    pub version: u32,
    pub device_cert: DeviceCert,
    pub room: String,
    pub workspace_id: u64,
    pub authority: DeviceCert,
    pub certified_roster: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    pub records: Vec<DocumentRow>,
    pub governance_messages: Vec<Vec<u8>>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "public wire enum: boxing a variant would change every constructor and match"
)]
pub enum JoinTransportResponse {
    Inspection(JoinInspection),
    Challenge(JoinChallengeResponse),
    Complete(JoinBootstrap),
    MembershipChallenge(MembershipChallengeResponse),
    MembershipComplete(JoinBootstrap),
    Error { code: u16, message: String },
    OwnerMembershipComplete(OwnerBootstrapV1),
}

/// Sign a delegation of workspace permissions from `authority` to `delegate`.
///
/// # Errors
///
/// Returns an error if the delegation is malformed or `authority_key` does not match
/// `authority`.
pub fn sign_workspace_delegation(
    room: impl Into<String>,
    workspace_id: u64,
    authority: &DeviceCert,
    delegate: DeviceCert,
    permissions: WorkspaceDelegationPermissions,
    nonce: [u8; 16],
    expires_at_ns: Option<u64>,
    authority_key: &ed25519_dalek::SigningKey,
) -> Result<WorkspaceDelegation, String> {
    if authority_key.verifying_key().to_bytes() != authority.device_public_key {
        return Err("delegation signer is not the room authority device".into());
    }
    let room = uuid::Uuid::parse_str(&room.into())
        .map_err(|_| "delegation room is not a UUID".to_string())?
        .to_string();
    let mut delegation = WorkspaceDelegation {
        version: JOIN_VERSION,
        room,
        workspace_id,
        authority_device_public_key: authority.device_public_key,
        delegate,
        permissions,
        nonce,
        expires_at_ns,
        signature: [0; 64],
    };
    let digest = tagged_digest(DELEGATION_DOMAIN, &delegation.view())?;
    delegation.signature = authority_key.sign(&digest).to_bytes();
    Ok(delegation)
}

impl WorkspaceDelegation {
    fn view(&self) -> DelegationView<'_> {
        DelegationView {
            version: self.version,
            room: &self.room,
            workspace_id: self.workspace_id,
            authority_device_public_key: &self.authority_device_public_key,
            delegate: &self.delegate,
            permissions: self.permissions,
            nonce: &self.nonce,
            expires_at_ns: self.expires_at_ns,
        }
    }
}

/// Verify a workspace delegation against `authority` and `revocations`.
///
/// # Errors
///
/// Returns an error if the signature, certificates, expiry or revocation state does not
/// verify at `now_ns`.
pub fn verify_workspace_delegation(
    delegation: &WorkspaceDelegation,
    authority: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    if delegation.version != JOIN_VERSION
        || delegation.authority_device_public_key != authority.device_public_key
        || uuid::Uuid::parse_str(&delegation.room)
            .map_or(true, |room| room.to_string() != delegation.room)
    {
        return Err("workspace delegation binding is invalid".into());
    }
    if delegation
        .expires_at_ns
        .is_some_and(|expiry| now_ns >= expiry)
    {
        return Err("workspace delegation has expired".into());
    }
    validate_certificate(authority, revocations, now_ns)?;
    validate_certificate(&delegation.delegate, revocations, now_ns)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&authority.device_public_key)
        .map_err(|_| "invalid delegation authority key".to_string())?;
    let digest = tagged_digest(DELEGATION_DOMAIN, &delegation.view())?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&delegation.signature),
    )
    .map_err(|_| "workspace delegation signature did not verify".to_string())
}

/// Sign an invitation grant, returning its wire bytes.
///
/// # Errors
///
/// Returns an error if the grant is malformed or `key` does not match its issuer.
pub fn issue_invitation(
    grant: InviteGrant,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    validate_grant(&grant, 0, false)?;
    if key.verifying_key().to_bytes() != grant.issuer.device_public_key {
        return Err("invitation signer is not the declared issuer device".into());
    }
    let view = InvitationView {
        version: INVITATION_VERSION,
        grant: &grant,
    };
    let digest = tagged_digest(INVITATION_DOMAIN, &view)?;
    let signed = SignedInvitation {
        version: INVITATION_VERSION,
        grant,
        signature: key.sign(&digest).to_bytes(),
    };
    postcard::to_stdvec(&signed).map_err(|error| error.to_string())
}

/// [`issue_invitation`] that also names `inviter`, under the same signature.
///
/// # Errors
///
/// Returns an error if the grant or the inviter is malformed, or the signer
/// is not the declared issuer.
pub fn issue_invitation_from(
    grant: InviteGrant,
    inviter: Inviter,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    validate_grant(&grant, 0, false)?;
    validate_inviter(&inviter)?;
    if key.verifying_key().to_bytes() != grant.issuer.device_public_key {
        return Err("invitation signer is not the declared issuer device".into());
    }
    let digest = tagged_digest(
        INVITATION_DOMAIN,
        &InvitationWithInviterView {
            version: INVITATION_WITH_INVITER_VERSION,
            grant: &grant,
            inviter: &inviter,
        },
    )?;
    let signed = SignedInvitationWithInviter {
        version: INVITATION_WITH_INVITER_VERSION,
        grant,
        inviter,
        signature: key.sign(&digest).to_bytes(),
    };
    postcard::to_stdvec(&signed).map_err(|error| error.to_string())
}

fn validate_inviter(inviter: &Inviter) -> Result<(), String> {
    let name = inviter.name.trim();
    if name.is_empty()
        || name != inviter.name
        || name.chars().count() > MAX_INVITER_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err("invalid inviter name".into());
    }
    Ok(())
}

/// Sign an opaque transport hint as `host`.
///
/// # Errors
///
/// Returns an error if the hint is malformed or `host_key` does not match `host`.
pub fn sign_transport_hint(
    kind: impl Into<String>,
    opaque_json: impl Into<String>,
    host: DeviceCert,
    service_public_key: [u8; 32],
    host_key: &ed25519_dalek::SigningKey,
) -> Result<TransportHint, String> {
    if host_key.verifying_key().to_bytes() != host.device_public_key {
        return Err("transport hint signer is not the certified host device".into());
    }
    let kind = kind.into();
    let opaque_json = opaque_json.into();
    let digest = tagged_digest(
        TRANSPORT_PIN_DOMAIN,
        &TransportPinView {
            kind: &kind,
            opaque_json: &opaque_json,
            service_public_key: &service_public_key,
        },
    )?;
    Ok(TransportHint {
        kind,
        opaque_json,
        host,
        service_public_key,
        host_signature: host_key.sign(&digest).to_bytes(),
    })
}

/// Verify a transport hint's signature and host certificate.
///
/// # Errors
///
/// Returns an error if the hint does not verify.
pub fn verify_transport_hint(hint: &TransportHint) -> Result<(), String> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(&hint.host.device_public_key)
        .map_err(|_| "invalid transport host device key".to_string())?;
    let digest = tagged_digest(
        TRANSPORT_PIN_DOMAIN,
        &TransportPinView {
            kind: &hint.kind,
            opaque_json: &hint.opaque_json,
            service_public_key: &hint.service_public_key,
        },
    )?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&hint.host_signature),
    )
    .map_err(|_| "transport host did not sign the endpoint service key".to_string())
}

/// Verify a transport hint at `now_ns` against `revocations`.
///
/// # Errors
///
/// Returns an error if the hint does not verify, has expired or its host is revoked.
pub fn verify_transport_hint_at(
    hint: &TransportHint,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    validate_certificate(&hint.host, revocations, now_ns)?;
    verify_transport_hint(hint)
}

pub fn encode_invitation_text(signed_invitation: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signed_invitation)
}

/// Decode a textual invitation and verify it.
///
/// # Errors
///
/// Returns an error if the text is not a valid invitation encoding or verification
/// fails.
pub fn decode_and_verify_invitation_text(
    text: &str,
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedJoin, String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| "invalid invitation text encoding".to_string())?;
    decode_and_verify_invitation(&bytes, recipient_user_public_key, revocations, now_ns)
}

/// Decode a signed invitation and verify it for the recipient.
///
/// # Errors
///
/// Returns an error if the invitation is malformed, not for the recipient, expired,
/// revoked, or its signature does not verify.
pub fn decode_and_verify_invitation(
    signed_invitation: &[u8],
    recipient_user_public_key: Option<[u8; 32]>,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedJoin, String> {
    let malformed = |_| "invalid signed invitation".to_string();
    let (version, _) = postcard::take_from_bytes::<u32>(signed_invitation).map_err(malformed)?;
    let (grant, inviter, signature) = match version {
        INVITATION_VERSION => {
            let signed: SignedInvitation =
                postcard::from_bytes(signed_invitation).map_err(malformed)?;
            (signed.grant, None, signed.signature)
        }
        INVITATION_WITH_INVITER_VERSION => {
            let signed: SignedInvitationWithInviter =
                postcard::from_bytes(signed_invitation).map_err(malformed)?;
            validate_inviter(&signed.inviter)?;
            (signed.grant, Some(signed.inviter), signed.signature)
        }
        _ => return Err("unsupported invitation version".into()),
    };
    validate_grant(&grant, now_ns, true)?;
    validate_certificate(&grant.authority, revocations, now_ns)?;
    validate_certificate(&grant.issuer, revocations, now_ns)?;
    validate_invitation_delegation(&grant, revocations, now_ns)?;
    for hint in &grant.transport {
        verify_transport_hint_at(hint, revocations, now_ns)?;
    }
    if let Some(expected) = recipient_user_public_key
        && grant
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != expected)
    {
        return Err("invitation recipient mismatch".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&grant.issuer.device_public_key)
        .map_err(|_| "invalid invitation issuer device key".to_string())?;
    let digest = match &inviter {
        None => tagged_digest(
            INVITATION_DOMAIN,
            &InvitationView {
                version,
                grant: &grant,
            },
        )?,
        Some(inviter) => tagged_digest(
            INVITATION_DOMAIN,
            &InvitationWithInviterView {
                version,
                grant: &grant,
                inviter,
            },
        )?,
    };
    key.verify(&digest, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| "invitation signature did not verify".to_string())?;
    Ok(VerifiedJoin {
        invite_id: *blake3::hash(signed_invitation).as_bytes(),
        grant,
        inviter,
        signed_invitation: signed_invitation.to_vec(),
    })
}

/// Sign the answer to a join challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing.
pub fn sign_join_proof(
    challenge: JoinChallenge,
    key: &ed25519_dalek::SigningKey,
) -> Result<JoinProof, String> {
    if key.verifying_key().to_bytes() != challenge.device_public_key {
        return Err("join proof signer is not the challenged device".into());
    }
    let digest = tagged_digest(JOIN_PROOF_DOMAIN, &challenge)?;
    Ok(JoinProof {
        challenge,
        signature: key.sign(&digest).to_bytes(),
    })
}

/// Verify a join proof against the challenge that was issued.
///
/// # Errors
///
/// Returns an error if the proof does not match the challenge, its signature or device
/// certificate does not verify, or the device is revoked.
pub fn verify_join_proof(
    proof: &JoinProof,
    issued_challenge: &JoinChallenge,
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    if proof.challenge != *issued_challenge {
        return Err("join proof does not match the issued challenge".into());
    }
    if proof.challenge.version != JOIN_VERSION || now_ns >= proof.challenge.expires_at_ns {
        return Err("join challenge is expired or unsupported".into());
    }
    validate_certificate(device_cert, revocations, now_ns)?;
    if device_cert.user_public_key != proof.challenge.user_public_key
        || device_cert.device_public_key != proof.challenge.device_public_key
    {
        return Err("join proof identity does not match device certificate".into());
    }
    let join = decode_and_verify_invitation(
        &proof.challenge.signed_invitation,
        Some(device_cert.user_public_key),
        revocations,
        now_ns,
    )?;
    if join.grant.room != proof.challenge.room
        || join.grant.workspace_id != proof.challenge.workspace_id
        || join.grant.authority.device_public_key != proof.challenge.authority_device_public_key
    {
        return Err("join challenge does not match signed invitation".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid join device public key".to_string())?;
    let digest = tagged_digest(JOIN_PROOF_DOMAIN, &proof.challenge)?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&proof.signature),
    )
    .map_err(|_| "join proof signature did not verify".to_string())?;
    Ok(VerifiedRedemption {
        join,
        certificate: device_cert.clone(),
        request_id: proof.challenge.request_id,
    })
}

/// Sign the answer to a membership challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing.
pub fn sign_membership_proof(
    challenge: MembershipChallenge,
    key: &ed25519_dalek::SigningKey,
) -> Result<MembershipProof, String> {
    if key.verifying_key().to_bytes() != challenge.device_public_key {
        return Err("membership proof signer is not the challenged device".into());
    }
    let digest = tagged_digest(MEMBERSHIP_PROOF_DOMAIN, &challenge)?;
    Ok(MembershipProof {
        challenge,
        signature: key.sign(&digest).to_bytes(),
    })
}

/// Verify a membership proof against the challenge that was issued.
///
/// # Errors
///
/// Returns an error if the proof does not match the challenge, its signature or device
/// certificate does not verify, or the device is revoked.
pub fn verify_membership_proof(
    proof: &MembershipProof,
    issued_challenge: &MembershipChallenge,
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedMembership, String> {
    if proof.challenge != *issued_challenge {
        return Err("membership proof does not match the issued challenge".into());
    }
    if proof.challenge.version != JOIN_VERSION || now_ns >= proof.challenge.expires_at_ns {
        return Err("membership challenge is expired or unsupported".into());
    }
    validate_certificate(device_cert, revocations, now_ns)?;
    if device_cert.user_public_key != proof.challenge.user_public_key
        || device_cert.device_public_key != proof.challenge.device_public_key
    {
        return Err("membership proof identity does not match device certificate".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid membership device public key".to_string())?;
    let digest = tagged_digest(MEMBERSHIP_PROOF_DOMAIN, &proof.challenge)?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&proof.signature),
    )
    .map_err(|_| "membership proof signature did not verify".to_string())?;
    Ok(VerifiedMembership {
        challenge: proof.challenge.clone(),
        device_cert: device_cert.clone(),
    })
}

fn validate_grant(grant: &InviteGrant, now_ns: u64, enforce_expiry: bool) -> Result<(), String> {
    let canonical = uuid::Uuid::parse_str(&grant.room)
        .map_err(|_| "invitation room is not a UUID".to_string())?
        .to_string();
    if canonical != grant.room {
        return Err("invitation room is not canonical".into());
    }
    if enforce_expiry && now_ns >= grant.expires_at_ns {
        return Err("invitation has expired".into());
    }
    if grant.issuer != grant.authority && grant.delegation.is_none() {
        return Err("delegated invitation is missing workspace delegation".into());
    }
    if grant.issuer == grant.authority && grant.delegation.is_some() {
        return Err("direct authority invitation must not carry delegation".into());
    }
    if grant.transport.len() > 32
        || grant.transport.iter().any(|hint| {
            hint.kind.is_empty()
                || hint.kind.len() > 128
                || hint.opaque_json.len() > 64 * 1024
                || serde_json::from_str::<serde_json::Value>(&hint.opaque_json).is_err()
        })
    {
        return Err("invalid invitation transport hints".into());
    }
    if grant
        .invitee_name
        .as_ref()
        .is_some_and(|name| name.len() > 1024)
    {
        return Err("invitation display name is too long".into());
    }
    Ok(())
}

fn validate_invitation_delegation(
    grant: &InviteGrant,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    let Some(delegation) = &grant.delegation else {
        return if grant.issuer == grant.authority {
            Ok(())
        } else {
            Err("delegated invitation is missing workspace delegation".into())
        };
    };
    verify_workspace_delegation(delegation, &grant.authority, revocations, now_ns)?;
    // The delegate is the invitation's issuer, not another delegate.
    let issued_by_delegate = delegation.delegate == grant.issuer;
    if delegation.room != grant.room
        || delegation.workspace_id != grant.workspace_id
        || !issued_by_delegate
        || !delegation.permissions.issue_invitations
        || !delegation.permissions.admit_invited_members
        || delegation
            .expires_at_ns
            .is_some_and(|expiry| grant.expires_at_ns > expiry)
    {
        return Err("workspace delegation does not authorize this invitation".into());
    }
    Ok(())
}

fn validate_certificate(
    certificate: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(certificate, now_ns)
        .map_err(|error| error.to_string())?;
    if revocations.is_revoked(&certificate.device_public_key) {
        return Err("device certificate is revoked".into());
    }
    Ok(())
}

fn tagged_digest(value: &[u8], body: &impl Serialize) -> Result<[u8; 32], String> {
    let encoded = postcard::to_stdvec(body).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(value);
    hasher.update(&encoded);
    Ok(*hasher.finalize().as_bytes())
}
