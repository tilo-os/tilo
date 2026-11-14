//! The invitation and join layouts issued before the grants stopped naming
//! the workspace's authority, kept so bytes a build wrote then still decode.
//! They name the workspace the way [`super::room_layout`] does: by a number
//! and by its UUID as text.
//! Their fields keep the names that layout had: the structs exist to mirror
//! the old bytes, not to name today's.
//!
//! Only the layout of the signed bytes changed: the signing domains are the
//! same, so an old signature verifies over the old layout. A stored record
//! keeps those bytes (they are what its device signed, and nothing here can
//! sign for it again), and [`super::verify_stored_complete`] reaches this
//! module because the challenge version names it, never by guessing.

use super::room_layout::workspace_of;
use super::{
    Decoded, DeviceCert, InviteGrant, Inviter, Role, TransportHint, VerifiedRedemption,
    tagged_digest, validate_certificate, validate_inviter, verify_issuer_signature,
};
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert as Cert, RevocationList};

/// The invitation version of a grant that names its authority.
pub(super) const INVITATION_VERSION: u32 = 1;
/// [`INVITATION_VERSION`] that also names who sent it.
pub(super) const INVITATION_WITH_INVITER_VERSION: u32 = 2;
/// The join version of challenges that name the challenged authority.
pub(super) const JOIN_VERSION: u32 = 1;
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
    role: Role,
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

impl Grant {
    fn into_current(self) -> Result<InviteGrant, String> {
        Ok(InviteGrant {
            workspace_id: workspace_of(&self.room)?,
            issuer: self.issuer,
            role: self.role,
            nonce: self.nonce,
            recipient_user_public_key: self.recipient_user_public_key,
            invitee_name: self.invitee_name,
            expires_at_ns: self.expires_at_ns,
            transport: self.transport,
        })
    }
}

/// A grant this module's layout wrote, read on its own: the copy a host keeps
/// beside the invitation it signed, which a governor's join ledger holds once
/// per invitation issued. It is the same grant [`decode_invitation`] reads out
/// of the signed bytes, so this layout is the one the invitation's version
/// names.
///
/// Answers the grant and the bytes it took, so a reader of a record that
/// keeps more after it knows where the grant ends.
///
/// # Errors
///
/// The bytes are not a grant of this layout.
pub fn take_stored_grant(bytes: &[u8]) -> Result<(InviteGrant, usize), String> {
    let (grant, tail): (Grant, &[u8]) = postcard::take_from_bytes(bytes)
        .map_err(|_| "invalid stored invitation grant".to_string())?;
    let taken = bytes
        .len()
        .checked_sub(tail.len())
        .ok_or_else(|| "invalid stored invitation grant".to_string())?;
    Ok((grant.into_current()?, taken))
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
    workspace_of(&grant.room)?;
    check_delegation(&grant)?;
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
    verify_issuer_signature(&grant.issuer, &digest, &signature)?;
    let authority = grant.authority.clone();
    let workspace_number = grant.workspace_id;
    Ok(Decoded {
        grant: grant.into_current()?,
        inviter,
        // The device the workspace's host was when this was signed: the
        // issuer itself, or the authority that delegated the invitation to it.
        authority: Some(authority),
        workspace_number: Some(workspace_number),
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
    // The delegate is the device the authority let issue; the invitation
    // carries that device as its issuer.
    let delegate = &delegation.delegate;
    if delegation.version != JOIN_VERSION
        || delegation.authority_device_public_key != grant.authority.device_public_key
        || uuid::Uuid::parse_str(&delegation.room)
            .map_or(true, |workspace| workspace.to_string() != delegation.room)
        || delegation.room != grant.room
        || delegation.workspace_id != grant.workspace_id
        || delegate != &grant.issuer
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
    let (variant, complete): (u32, JoinCompleteRequest) =
        postcard::from_bytes(bytes).map_err(|_| "invalid stored Complete".to_string())?;
    if variant != super::COMPLETE_VARIANT {
        return Err("stored join proof is not Complete".into());
    }
    if postcard::to_stdvec(&(variant, &complete)).map_err(|error| error.to_string())? != bytes {
        return Err("noncanonical stored Complete".into());
    }
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
    if workspace_of(&challenge.room)? != join.grant.workspace_id
        || join
            .workspace_number
            .is_some_and(|number| number != challenge.workspace_id)
        || join.workspace_authority().device_public_key != challenge.authority_device_public_key
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
    if expected.workspace_id != join.grant.workspace_id
        || expected.signed_invitation != challenge.signed_invitation
        || expected.user_public_key != challenge.user_public_key
        || expected.device_public_key != challenge.device_public_key
        || expected.request_id != challenge.request_id
        || expected.nonce != challenge.nonce
        || expected.expires_at_ns != challenge.expires_at_ns
    {
        return Err("join challenge scope mismatch".into());
    }
    super::verify_device_proof(
        device_cert,
        &tagged_digest(super::JOIN_PROOF_DOMAIN, &challenge)?,
        &complete.proof.signature,
    )?;
    Ok(VerifiedRedemption {
        join,
        certificate: device_cert.clone(),
        request_id: challenge.request_id,
    })
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
        workspace_id: super::room_layout::workspace_number(grant.workspace_id),
        room: grant.workspace_id.to_string(),
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
        room: challenge.workspace_id.to_string(),
        workspace_id: super::room_layout::workspace_number(challenge.workspace_id),
        authority_device_public_key: authority.device_public_key,
        user_public_key: challenge.user_public_key,
        device_public_key: challenge.device_public_key,
        request_id: challenge.request_id,
        nonce: challenge.nonce,
        expires_at_ns: challenge.expires_at_ns,
    };
    let digest = tagged_digest(super::JOIN_PROOF_DOMAIN, &prior)?;
    postcard::to_stdvec(&(
        super::COMPLETE_VARIANT,
        JoinCompleteRequest {
            proof: JoinProof {
                challenge: prior,
                signature: key.sign(&digest).to_bytes(),
            },
        },
    ))
    .map_err(|error| error.to_string())
}
