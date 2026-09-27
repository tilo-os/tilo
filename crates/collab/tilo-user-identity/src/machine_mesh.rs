//! Bilateral, action-scoped machine associations. Never grants workspace membership.
#![allow(missing_docs)]

use crate::{DeviceCert, verify_device_cert_at};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Grant scope for invoking the target's broker `NativeOperation`s over a link.
///
/// Replaces the retired AI-only `model_complete` label. Links still never grant
/// workspace membership; the target checks the source device's ordinary permissions.
pub const BROKER_ACTIONS_SCOPE: &str = "broker_actions";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshChallenge {
    pub version: u32,
    pub machine_id: String,
    pub room: String,
    pub root_public_key: [u8; 32],
    pub host_certificate: DeviceCert,
    /// Exact serialized `EndpointAddr`; opaque to browser and control plane.
    pub action_address: String,
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshRequest {
    pub version: u32,
    pub kind: String,
    pub link_id: String,
    pub source: MeshChallenge,
    pub target: MeshChallenge,
    pub allowed_actions: Vec<String>,
    pub source_signature: Vec<u8>,
    pub target_signature: Vec<u8>,
}

fn digest(request: &MeshRequest) -> Result<[u8; 32], String> {
    let bytes = postcard::to_allocvec(&(
        request.version,
        &request.kind,
        &request.link_id,
        &request.source,
        &request.target,
        &request.allowed_actions,
    ))
    .map_err(|error| error.to_string())?;
    Ok(blake3::derive_key(
        "tilo bilateral machine actions v1",
        &bytes,
    ))
}

/// Sign a bilateral link request with both machines' root keys.
///
/// # Errors
///
/// Returns a message when either key is not the root named by its challenge,
/// or when the request cannot be serialized for signing.
pub fn sign_request(
    request: &mut MeshRequest,
    source: &SigningKey,
    target: &SigningKey,
) -> Result<(), String> {
    if source.verifying_key().to_bytes() != request.source.root_public_key
        || target.verifying_key().to_bytes() != request.target.root_public_key
    {
        return Err("Existing machine root approval required".into());
    }
    let bytes = digest(request)?;
    request.source_signature = source.sign(&bytes).to_bytes().to_vec();
    request.target_signature = target.sign(&bytes).to_bytes().to_vec();
    Ok(())
}

/// Verify a bilateral link request against both challenges at `now`.
///
/// # Errors
///
/// Returns a message for a malformed or expired request, or when either root
/// signature does not verify.
pub fn verify_request(request: &MeshRequest, now: u64) -> Result<(), String> {
    if request.version != 1
        || !matches!(request.kind.as_str(), "link" | "unlink")
        || request.link_id.is_empty()
        || request.link_id.len() > 128
        || !request
            .link_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        || request.source.machine_id == request.target.machine_id
        || request.source.host_certificate.device_public_key
            == request.target.host_certificate.device_public_key
        || request.allowed_actions != [BROKER_ACTIONS_SCOPE]
    {
        return Err("Invalid or unsupported mesh grant".into());
    }
    for challenge in [&request.source, &request.target] {
        if challenge.version != 1
            || challenge.machine_id.is_empty()
            || challenge.machine_id.len() > 128
            || !challenge
                .machine_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            || challenge.room.is_empty()
            || challenge.room.len() > 128
            || (request.kind == "link"
                && (now >= challenge.expires_at_ns
                    || challenge.expires_at_ns > now.saturating_add(300_000_000_000)))
            || challenge.action_address.len() > 4096
            || challenge.root_public_key != challenge.host_certificate.user_public_key
            || !challenge.host_certificate.capabilities.can_act_as_host
        {
            return Err("Invalid or expired mesh challenge".into());
        }
        verify_device_cert_at(&challenge.host_certificate, now)
            .map_err(|error| error.to_string())?;
    }
    let bytes = digest(request)?;
    for (key, signature) in [
        (request.source.root_public_key, &request.source_signature),
        (request.target.root_public_key, &request.target_signature),
    ] {
        VerifyingKey::from_bytes(&key)
            .map_err(|error| error.to_string())?
            .verify_strict(
                &bytes,
                &Signature::from_slice(signature).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
