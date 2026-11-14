//! The invitation and join layouts that named a workspace twice: by a 64-bit
//! number derived from its UUID and by the UUID as text (`room`), kept so the
//! bytes the 0.1 releases wrote still decode. Their fields keep the names that
//! layout had: the structs mirror the old bytes, not today's.
//!
//! Only the layout of the signed bytes changed, not the signing domains, so a
//! signature made then verifies over this layout. A stored record keeps those
//! bytes (an invitation's id is their digest, and nothing here can sign for its
//! issuer again), and the version they carry is what sends them here.

use super::{
    Decoded, DeviceCert, InviteGrant, Inviter, Role, TransportHint, VerifiedRedemption,
    WorkspaceId, tagged_digest, validate_certificate, validate_inviter, verify_issuer_signature,
};
use serde::{Deserialize, Serialize};
use tilo_user_identity::RevocationList;

/// An invitation whose grant names its workspace by number and text.
pub(super) const INVITATION_VERSION: u32 = 3;
/// [`INVITATION_VERSION`] that also names who sent it.
pub(super) const INVITATION_WITH_INVITER_VERSION: u32 = 4;
/// A join challenge that names its workspace by text and number.
pub(super) const JOIN_VERSION: u32 = 2;

/// Whether `version` is an invitation this module decodes.
pub(super) const fn is_invitation_version(version: u32) -> bool {
    matches!(
        version,
        INVITATION_VERSION | INVITATION_WITH_INVITER_VERSION
    )
}

/// The workspace a grant or challenge of this layout names by `room`: its
/// UUID in the canonical lowercase text the releases required.
pub(super) fn workspace_of(room: &str) -> Result<WorkspaceId, String> {
    WorkspaceId::parse(room)
        .filter(|workspace| workspace.to_string() == room)
        .ok_or_else(|| "invitation workspace is not a canonical UUID".to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Grant {
    workspace_id: u64,
    room: String,
    issuer: DeviceCert,
    role: Role,
    nonce: [u8; 16],
    recipient_user_public_key: Option<[u8; 32]>,
    invitee_name: Option<String>,
    expires_at_ns: u64,
    transport: Vec<TransportHint>,
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

/// The grant of a signed invitation in this layout, read but not checked.
fn read_grant(bytes: &[u8], version: u32) -> Result<(Grant, Option<Inviter>, [u8; 64]), String> {
    let malformed = |_| "invalid signed invitation".to_string();
    match version {
        INVITATION_VERSION => {
            let signed: SignedInvitation = postcard::from_bytes(bytes).map_err(malformed)?;
            Ok((signed.grant, None, signed.signature))
        }
        INVITATION_WITH_INVITER_VERSION => {
            let signed: SignedInvitationWithInviter =
                postcard::from_bytes(bytes).map_err(malformed)?;
            Ok((signed.grant, Some(signed.inviter), signed.signature))
        }
        _ => Err("unsupported invitation version".into()),
    }
}

/// A signed invitation of this layout as this build reads and writes it
/// back: the same bytes when the layout's types still encode as that day.
#[cfg(test)]
pub(super) fn rewritten_invitation(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let (version, _) =
        postcard::take_from_bytes::<u32>(bytes).map_err(|error| error.to_string())?;
    let (grant, inviter, signature) = read_grant(bytes, version)?;
    match inviter {
        None => postcard::to_stdvec(&SignedInvitation {
            version,
            grant,
            signature,
        }),
        Some(inviter) => postcard::to_stdvec(&SignedInvitationWithInviter {
            version,
            grant,
            inviter,
            signature,
        }),
    }
    .map_err(|error| error.to_string())
}

/// The grant a signed invitation of this layout carries, unchecked: for
/// naming the runs it shares with others, never for admitting anyone.
pub(super) fn unchecked_grant(bytes: &[u8], version: u32) -> Option<InviteGrant> {
    read_grant(bytes, version)
        .ok()
        .and_then(|(grant, _, _)| grant.into_current().ok())
}

/// Decode and check the issuer's signature of a signed invitation in the
/// layout its `version` names.
pub(super) fn decode_invitation(bytes: &[u8], version: u32) -> Result<Decoded, String> {
    let (grant, inviter, signature) = read_grant(bytes, version)?;
    workspace_of(&grant.room)?;
    if let Some(inviter) = &inviter {
        validate_inviter(inviter)?;
    }
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
    let workspace_number = grant.workspace_id;
    Ok(Decoded {
        grant: grant.into_current()?,
        inviter,
        authority: None,
        workspace_number: Some(workspace_number),
    })
}

/// Decode and check a stored `JoinTransportRequest::Complete` whose challenge
/// is in this module's layout.
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
    // The challenge a host of this layout issued, made again from the
    // invitation and the device: every field but its version and its way of
    // naming the workspace must be what was signed.
    let expected = super::JoinChallenge::new(
        &join,
        device_cert,
        challenge.request_id,
        challenge.nonce,
        challenge.expires_at_ns,
        now_ns,
    )?;
    if workspace_of(&challenge.room)? != expected.workspace_id
        || join
            .workspace_number
            .is_some_and(|number| number != challenge.workspace_id)
        || expected.signed_invitation != challenge.signed_invitation
        || expected.user_public_key != challenge.user_public_key
        || expected.device_public_key != challenge.device_public_key
        || expected.request_id != challenge.request_id
        || expected.nonce != challenge.nonce
        || expected.expires_at_ns != challenge.expires_at_ns
    {
        return Err("join challenge does not match signed invitation".into());
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

/// The number the 0.1 releases derived from a workspace's UUID and carried
/// beside it.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn workspace_number(workspace: WorkspaceId) -> u64 {
    let hash = blake3::derive_key("tilo-room-transport-v1", workspace.to_string().as_bytes());
    match u64::from_le_bytes(hash.first_chunk::<8>().copied().unwrap_or_default()) {
        0 => 1,
        id => id,
    }
}

/// An invitation of this module's layout, signed by `key` the way the 0.1
/// releases signed it: for replaying a record they left.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn encode_invitation(
    grant: &InviteGrant,
    inviter: Option<&Inviter>,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer as _;
    let version = if inviter.is_some() {
        INVITATION_WITH_INVITER_VERSION
    } else {
        INVITATION_VERSION
    };
    let prior = Grant {
        workspace_id: workspace_number(grant.workspace_id),
        room: grant.workspace_id.to_string(),
        issuer: grant.issuer.clone(),
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

/// A stored Complete in this module's layout, answering `challenge` and
/// signed by `key`: the proof a 0.1 release kept.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn encode_complete(
    challenge: &super::JoinChallenge,
    key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, String> {
    use ed25519_dalek::Signer as _;
    let prior = JoinChallenge {
        version: JOIN_VERSION,
        signed_invitation: challenge.signed_invitation.clone(),
        room: challenge.workspace_id.to_string(),
        workspace_id: workspace_number(challenge.workspace_id),
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
