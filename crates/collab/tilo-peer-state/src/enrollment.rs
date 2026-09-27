//! Public enrollment proof contract. Secrets never enter server bootstrap data.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
const DOMAIN: &[u8] = b"tilo/browser-enrollment/v2";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub device_public_key: String,
    pub request_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeResponse {
    pub version: u32,
    pub challenge: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRequest {
    pub version: u32,
    pub challenge: String,
    pub signature: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentChallenge {
    pub version: u32,
    pub root_public_key: [u8; 32],
    pub room: String,
    pub workspace_id: u64,
    pub device_public_key: [u8; 32],
    pub request_id: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at_ns: u64,
}
fn bytes(value: &EnrollmentChallenge) -> Result<Vec<u8>, String> {
    if value.version != 2 || value.room.len() > 128 || uuid::Uuid::parse_str(&value.room).is_err() {
        return Err("invalid enrollment challenge".into());
    }
    postcard::to_allocvec(value).map_err(|e| e.to_string())
}
/// Text encoding of an enrollment challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be serialized.
pub fn encode(value: &EnrollmentChallenge) -> Result<String, String> {
    Ok(hex::encode(bytes(value)?))
}
/// Decode a textual enrollment challenge.
///
/// # Errors
///
/// Returns an error if the text is not a valid challenge encoding.
pub fn decode(encoded: &str) -> Result<EnrollmentChallenge, String> {
    if encoded.len() > 2048 {
        return Err("enrollment challenge too large".into());
    }
    let raw = hex::decode(encoded).map_err(|e| e.to_string())?;
    let value: EnrollmentChallenge = postcard::from_bytes(&raw).map_err(|e| e.to_string())?;
    if bytes(&value)? != raw {
        return Err("noncanonical enrollment challenge".into());
    }
    Ok(value)
}
fn preimage(value: &EnrollmentChallenge) -> Result<Vec<u8>, String> {
    let mut out = DOMAIN.to_vec();
    out.extend(bytes(value)?);
    Ok(out)
}
/// Sign an enrollment challenge.
///
/// # Errors
///
/// Returns an error if the challenge cannot be encoded for signing.
pub fn sign(value: &EnrollmentChallenge, key: &SigningKey) -> Result<CompleteRequest, String> {
    if key.verifying_key().to_bytes() != value.device_public_key {
        return Err("enrollment key mismatch".into());
    }
    Ok(CompleteRequest {
        version: 2,
        challenge: encode(value)?,
        signature: hex::encode(key.sign(&preimage(value)?).to_bytes()),
    })
}
/// Verify a completed enrollment for `root`, `room` and `workspace`.
///
/// # Errors
///
/// Returns an error if the signature, binding or expiry does not verify at `now_ns`.
pub fn verify(
    request: &CompleteRequest,
    owner_root: [u8; 32],
    room: &str,
    workspace: u64,
    now_ns: u64,
) -> Result<EnrollmentChallenge, String> {
    if request.version != 2 {
        return Err("unsupported enrollment proof version".into());
    }
    let value = decode(&request.challenge)?;
    if value.root_public_key != owner_root || value.room != room || value.workspace_id != workspace
    {
        return Err("enrollment scope mismatch".into());
    }
    if now_ns >= value.expires_at_ns {
        return Err("enrollment challenge expired".into());
    }
    let signature = hex::decode(&request.signature).map_err(|e| e.to_string())?;
    let signature = Signature::from_slice(&signature).map_err(|e| e.to_string())?;
    VerifyingKey::from_bytes(&value.device_public_key)
        .map_err(|e| e.to_string())?
        .verify_strict(&preimage(&value)?, &signature)
        .map_err(|e| e.to_string())?;
    Ok(value)
}
