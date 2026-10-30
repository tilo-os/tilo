//! The invitation and join layouts issued before the grants stopped naming
//! the room's authority, kept so bytes a build wrote then still decode.
//!
//! Only the layout of the signed bytes changed: the signing domains are the
//! same, so an old signature verifies over the old layout. A stored record
//! keeps those bytes (they are what its device signed, and nothing here can
//! sign for it again), and [`super::verify_stored_complete`] reaches this
//! module because the challenge version names it, never by guessing.

use super::{
    DeviceCert, InviteGrant, Inviter, JoinChallengeRequest, MemberRole, MembershipChallengeRequest,
    MembershipCompleteRequest, TransportHint, VerifiedJoin, VerifiedRedemption, tagged_digest,
    validate_certificate, validate_inviter,
};
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert as Cert, RevocationList};

/// The invitation version of a grant that names its authority.
const INVITATION_VERSION: u32 = 1;
/// [`INVITATION_VERSION`] that also names who sent it.
const INVITATION_WITH_INVITER_VERSION: u32 = 2;
/// The join version of challenges that name the challenged authority.
const JOIN_VERSION: u32 = 1;
const DELEGATION_DOMAIN: &[u8] = b"tilo/workspace-delegation/v1\0";

/// Whether `version` is an invitation this module decodes.
pub(super) const fn is_invitation_version(version: u32) -> bool {
    matches!(
        version,
        INVITATION_VERSION | INVITATION_WITH_INVITER_VERSION
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Grant {
    workspace_id: u64,
    room: String,
    authority: Cert,
    issuer: Cert,
    delegation: Option<Delegation>,
    role: MemberRole,
    nonce: [u8; 16],
    recipient_user_public_key: Option<[u8; 32]>,
    invitee_name: Option<String>,
    expires_at_ns: u64,
    transport: Vec<TransportHint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DelegationPermissions {
    issue_invitations: bool,
    admit_invited_members: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Delegation {
    version: u32,
    room: String,
    workspace_id: u64,
    authority_device_public_key: [u8; 32],
    delegate: Cert,
    permissions: DelegationPermissions,
    nonce: [u8; 16],
    expires_at_ns: Option<u64>,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Serialize)]
struct DelegationView<'a> {
    version: u32,
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: &'a [u8; 32],
    delegate: &'a Cert,
    permissions: DelegationPermissions,
    nonce: &'a [u8; 16],
    expires_at_ns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitation {
    version: u32,
    grant: Grant,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SignedInvitationWithInviter {
    version: u32,
    grant: Grant,
    inviter: Inviter,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Serialize)]
struct InvitationView<'a> {
    version: u32,
    grant: &'a Grant,
}

#[derive(Serialize)]
struct InvitationWithInviterView<'a> {
    version: u32,
    grant: &'a Grant,
    inviter: &'a Inviter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JoinChallenge {
    version: u32,
    signed_invitation: Vec<u8>,
    room: String,
    workspace_id: u64,
    authority_device_public_key: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    nonce: [u8; 32],
    expires_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JoinProof {
    challenge: JoinChallenge,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JoinCompleteRequest {
    proof: JoinProof,
}

/// A join request in the layout above. Only [`Self::Complete`] ever reaches
/// this module: a stored enrollment intent is a proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum JoinTransportRequest {
    Inspect { signed_invitation: Vec<u8> },
    Challenge(JoinChallengeRequest),
    Complete(JoinCompleteRequest),
    MembershipChallenge(MembershipChallengeRequest),
    MembershipComplete(MembershipCompleteRequest),
}

impl Grant {
    fn into_current(self) -> InviteGrant {
        InviteGrant {
            workspace_id: self.workspace_id,
            room: self.room,
            issuer: self.issuer,
            role: self.role,
            nonce: self.nonce,
            recipient_user_public_key: self.recipient_user_public_key,
            invitee_name: self.invitee_name,
            expires_at_ns: self.expires_at_ns,
            transport: self.transport,
        }
    }
}

/// A signed invitation of this layout, checked.
pub(super) struct Decoded {
    pub(super) grant: InviteGrant,
    /// The device the room's host was when this was signed: the issuer
    /// itself, or the authority that delegated the invitation to it.
    pub(super) authority: DeviceCert,
    pub(super) inviter: Option<Inviter>,
}

/// Decode and check the signature of a signed invitation in the layout its
/// `version` names.
pub(super) fn decode_invitation(bytes: &[u8], version: u32) -> Result<Decoded, String> {
    let malformed = |_| "invalid signed invitation".to_string();
    let (grant, inviter, signature) = match version {
        INVITATION_VERSION => {
            let signed: SignedInvitation = postcard::from_bytes(bytes).map_err(malformed)?;
            (signed.grant, None, signed.signature)
        }
        INVITATION_WITH_INVITER_VERSION => {
            let signed: SignedInvitationWithInviter =
                postcard::from_bytes(bytes).map_err(malformed)?;
            validate_inviter(&signed.inviter)?;
            (signed.grant, Some(signed.inviter), signed.signature)
        }
        _ => return Err("unsupported invitation version".into()),
    };
    check_delegation(&grant)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&grant.issuer.device_public_key)
        .map_err(|_| "invalid invitation issuer device key".to_string())?;
    let digest = match &inviter {
        None => tagged_digest(
            super::INVITATION_DOMAIN,
            &InvitationView {
                version,
                grant: &grant,
            },
        )?,
        Some(inviter) => tagged_digest(
            super::INVITATION_DOMAIN,
            &InvitationWithInviterView {
                version,
                grant: &grant,
                inviter,
            },
        )?,
    };
    key.verify(&digest, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| "invitation signature did not verify".to_string())?;
    let authority = grant.authority.clone();
    Ok(Decoded {
        grant: grant.into_current(),
        authority,
        inviter,
    })
}

/// An invitation of this layout named either its authority or a delegation
/// that let another device issue it: check the delegation the way the build
/// that signed it did, so the set of invitations this decodes is the set it
/// issued.
fn check_delegation(grant: &Grant) -> Result<(), String> {
    let Some(delegation) = &grant.delegation else {
        return if grant.issuer == grant.authority {
            Ok(())
        } else {
            Err("delegated invitation is missing workspace delegation".into())
        };
    };
    if delegation.version != JOIN_VERSION
        || delegation.authority_device_public_key != grant.authority.device_public_key
        || uuid::Uuid::parse_str(&delegation.room)
            .map_or(true, |room| room.to_string() != delegation.room)
        || delegation.room != grant.room
        || delegation.workspace_id != grant.workspace_id
        || delegation.delegate != grant.issuer
        || !delegation.permissions.issue_invitations
        || !delegation.permissions.admit_invited_members
        || delegation
            .expires_at_ns
            .is_some_and(|expiry| grant.expires_at_ns > expiry)
    {
        return Err("workspace delegation does not authorize this invitation".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&grant.authority.device_public_key)
        .map_err(|_| "invalid delegation authority key".to_string())?;
    let digest = tagged_digest(
        DELEGATION_DOMAIN,
        &DelegationView {
            version: delegation.version,
            room: &delegation.room,
            workspace_id: delegation.workspace_id,
            authority_device_public_key: &delegation.authority_device_public_key,
            delegate: &delegation.delegate,
            permissions: delegation.permissions,
            nonce: &delegation.nonce,
            expires_at_ns: delegation.expires_at_ns,
        },
    )?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&delegation.signature),
    )
    .map_err(|_| "workspace delegation signature did not verify".to_string())
}

/// Decode and check a stored `JoinTransportRequest::Complete` whose
/// challenge is in this module's layout.
pub(super) fn verify_complete(
    bytes: &[u8],
    device_cert: &DeviceCert,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<VerifiedRedemption, String> {
    let request: JoinTransportRequest =
        postcard::from_bytes(bytes).map_err(|_| "invalid stored Complete".to_string())?;
    if postcard::to_stdvec(&request).map_err(|error| error.to_string())? != bytes {
        return Err("noncanonical stored Complete".into());
    }
    let JoinTransportRequest::Complete(complete) = request else {
        return Err("stored join proof is not Complete".into());
    };
    let challenge = complete.proof.challenge;
    if challenge.version != JOIN_VERSION || now_ns >= challenge.expires_at_ns {
        return Err("join challenge is expired or unsupported".into());
    }
    validate_certificate(device_cert, revocations, now_ns)?;
    if device_cert.user_public_key != challenge.user_public_key
        || device_cert.device_public_key != challenge.device_public_key
    {
        return Err("join proof identity does not match device certificate".into());
    }
    let join = super::decode_and_verify_invitation(
        &challenge.signed_invitation,
        Some(device_cert.user_public_key),
        revocations,
        now_ns,
    )?;
    if join.grant.room != challenge.room
        || join.grant.workspace_id != challenge.workspace_id
        || join.room_authority().device_public_key != challenge.authority_device_public_key
    {
        return Err("join challenge does not match signed invitation".into());
    }
    // The challenge a host of this layout issues, made again from the
    // invitation and the device: every field but its version must be what
    // was signed.
    let expected = super::JoinChallenge::new(
        &join,
        device_cert,
        challenge.request_id,
        challenge.nonce,
        challenge.expires_at_ns,
        now_ns,
    )?;
    if expected.room != challenge.room
        || expected.workspace_id != challenge.workspace_id
        || expected.signed_invitation != challenge.signed_invitation
        || expected.user_public_key != challenge.user_public_key
        || expected.device_public_key != challenge.device_public_key
        || expected.request_id != challenge.request_id
        || expected.nonce != challenge.nonce
        || expected.expires_at_ns != challenge.expires_at_ns
    {
        return Err("join challenge scope mismatch".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&device_cert.device_public_key)
        .map_err(|_| "invalid join device public key".to_string())?;
    let digest = tagged_digest(super::JOIN_PROOF_DOMAIN, &challenge)?;
    key.verify(
        &digest,
        &ed25519_dalek::Signature::from_bytes(&complete.proof.signature),
    )
    .map_err(|_| "join proof signature did not verify".to_string())?;
    Ok(VerifiedRedemption {
        join,
        certificate: device_cert.clone(),
        request_id: challenge.request_id,
    })
}

impl VerifiedJoin {
    /// The join of a stored record, in the shape it was signed in.
    pub(super) fn from_decoded(decoded: Decoded, signed_invitation: Vec<u8>) -> Self {
        Self {
            invite_id: *blake3::hash(&signed_invitation).as_bytes(),
            grant: decoded.grant,
            inviter: decoded.inviter,
            signed_invitation,
            authority: Some(decoded.authority),
        }
    }
}

/// An invitation of this module's layout, signed: what a build wrote before
/// the version bump, for replaying the record it left.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn encode_invitation(
    grant: &InviteGrant,
    inviter: Option<&Inviter>,
    authority: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer as _;
    let version = if inviter.is_some() {
        INVITATION_WITH_INVITER_VERSION
    } else {
        INVITATION_VERSION
    };
    let prior = Grant {
        workspace_id: grant.workspace_id,
        room: grant.room.clone(),
        authority: authority.clone(),
        issuer: grant.issuer.clone(),
        delegation: None,
        role: grant.role,
        nonce: grant.nonce,
        recipient_user_public_key: grant.recipient_user_public_key,
        invitee_name: grant.invitee_name.clone(),
        expires_at_ns: grant.expires_at_ns,
        transport: grant.transport.clone(),
    };
    let digest = match inviter {
        None => tagged_digest(
            super::INVITATION_DOMAIN,
            &InvitationView {
                version,
                grant: &prior,
            },
        )?,
        Some(inviter) => tagged_digest(
            super::INVITATION_DOMAIN,
            &InvitationWithInviterView {
                version,
                grant: &prior,
                inviter,
            },
        )?,
    };
    let signature = key.sign(&digest).to_bytes();
    match inviter {
        None => postcard::to_stdvec(&SignedInvitation {
            version,
            grant: prior,
            signature,
        }),
        Some(inviter) => postcard::to_stdvec(&SignedInvitationWithInviter {
            version,
            grant: prior,
            inviter: inviter.clone(),
            signature,
        }),
    }
    .map_err(|error| error.to_string())
}

/// A stored Complete in this module's layout, signed by `key`: the proof a
/// build wrote before the join version bump.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn encode_complete(
    challenge: &super::JoinChallenge,
    authority: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer as _;
    let prior = JoinChallenge {
        version: JOIN_VERSION,
        signed_invitation: challenge.signed_invitation.clone(),
        room: challenge.room.clone(),
        workspace_id: challenge.workspace_id,
        authority_device_public_key: authority.device_public_key,
        user_public_key: challenge.user_public_key,
        device_public_key: challenge.device_public_key,
        request_id: challenge.request_id,
        nonce: challenge.nonce,
        expires_at_ns: challenge.expires_at_ns,
    };
    let digest = tagged_digest(super::JOIN_PROOF_DOMAIN, &prior)?;
    postcard::to_stdvec(&JoinTransportRequest::Complete(JoinCompleteRequest {
        proof: JoinProof {
            challenge: prior,
            signature: key.sign(&digest).to_bytes(),
        },
    }))
    .map_err(|error| error.to_string())
}
