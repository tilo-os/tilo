use ed25519_dalek::{Signer as _, Verifier as _};
use neighbour_wire::contribution::{
    CONTRIBUTION_DELEGATION_VERSION, ContributionAttestationDelegationV1,
};
use tilo_user_identity::{DeviceCert, RevocationList};

const DOMAIN: &[u8] = b"tilo/contribution-attestation-delegation/v1\0";

#[derive(serde::Serialize)]
struct SigningBody<'a> {
    version: u16,
    room: &'a str,
    workspace_id: u64,
    authority_device_public_key: &'a [u8; 32],
    delegate: &'a DeviceCert,
    nonce: &'a [u8; 16],
    expires_at_ns: Option<u64>,
}

fn signing_body(value: &ContributionAttestationDelegationV1) -> SigningBody<'_> {
    SigningBody {
        version: value.version,
        room: &value.room,
        workspace_id: value.workspace_id,
        authority_device_public_key: &value.authority_device_public_key,
        delegate: &value.delegate,
        nonce: &value.nonce,
        expires_at_ns: value.expires_at_ns,
    }
}

fn digest(value: &ContributionAttestationDelegationV1) -> Result<[u8; 32], String> {
    let bytes = postcard::to_allocvec(&signing_body(value)).map_err(|error| error.to_string())?;
    let mut hash = blake3::Hasher::new();
    hash.update(DOMAIN);
    hash.update(&bytes);
    Ok(*hash.finalize().as_bytes())
}

pub(super) fn validate_certificate(
    certificate: &DeviceCert,
    revocations: &RevocationList,
    at_ns: u64,
) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(certificate, at_ns)
        .map_err(|error| error.to_string())?;
    if revocations.is_revoked_at(&certificate.device_public_key, at_ns) {
        return Err("contribution attestation certificate was revoked".into());
    }
    Ok(())
}

fn canonical_room(room: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(room)
        .map_err(|_| "contribution delegation room is not a UUID".to_owned())
        .map(|room| room.to_string())
}

fn validate_binding(value: &ContributionAttestationDelegationV1) -> Result<(), String> {
    if value.version != CONTRIBUTION_DELEGATION_VERSION
        || value.room != canonical_room(&value.room)?
        || value.workspace_id == 0
        || tilo_relay_protocol::room_workspace_id(&value.room).map_err(|error| error.to_string())?
            != value.workspace_id
    {
        return Err("invalid contribution delegation binding".into());
    }
    Ok(())
}

/// Mint a governor-signed room-scoped contribution delegation.
///
/// # Errors
/// The room/workspace, certificates, expiry or signing custody is invalid.
pub fn sign_contribution_delegation(
    room: &str,
    workspace_id: u64,
    authority: &DeviceCert,
    delegate: DeviceCert,
    nonce: [u8; 16],
    expires_at_ns: Option<u64>,
    authority_key: &ed25519_dalek::SigningKey,
) -> Result<ContributionAttestationDelegationV1, String> {
    if authority_key.verifying_key().to_bytes() != authority.device_public_key
        || expires_at_ns.is_some_and(|expiry| expiry <= delegate.issued_at_ns)
    {
        return Err("invalid contribution delegation signing custody".into());
    }
    tilo_user_identity::verify_device_cert(authority).map_err(|error| error.to_string())?;
    tilo_user_identity::verify_device_cert(&delegate).map_err(|error| error.to_string())?;
    let mut value = ContributionAttestationDelegationV1 {
        version: CONTRIBUTION_DELEGATION_VERSION,
        room: canonical_room(room)?,
        workspace_id,
        authority_device_public_key: authority.device_public_key,
        delegate,
        nonce,
        expires_at_ns,
        signature: collab_types::Signature::from_bytes([0; 64]),
    };
    validate_binding(&value)?;
    value.signature =
        collab_types::Signature::from_bytes(authority_key.sign(&digest(&value)?).to_bytes());
    Ok(value)
}

/// Verify a contribution delegation against the exact enrolled governor.
///
/// # Errors
/// Any binding, certificate, revocation, expiry or signature is invalid.
pub fn verify_contribution_delegation(
    value: &ContributionAttestationDelegationV1,
    authority: &DeviceCert,
    revocations: &RevocationList,
    accepted_at_ns: u64,
) -> Result<(), String> {
    validate_binding(value)?;
    if value.authority_device_public_key != authority.device_public_key
        || value
            .expires_at_ns
            .is_some_and(|expiry| accepted_at_ns >= expiry)
    {
        return Err("contribution delegation is outside current authority".into());
    }
    validate_certificate(authority, revocations, accepted_at_ns)?;
    validate_certificate(&value.delegate, revocations, accepted_at_ns)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&authority.device_public_key)
        .map_err(|_| "invalid contribution governor key")?;
    key.verify(
        &digest(value)?,
        &ed25519_dalek::Signature::from_bytes(value.signature.as_bytes()),
    )
    .map_err(|_| "contribution delegation signature did not verify".into())
}
