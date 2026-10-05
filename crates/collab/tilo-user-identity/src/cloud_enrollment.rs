//! Browser-root-approved cloud enrollment. No secret material is serialized.

use crate::{DeviceCapabilities, DeviceCert, verify_device_cert_at};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Public challenge issued by the already provisioned machine. Field order is signed ABI.
pub struct Challenge {
    /// Wire version; currently one.
    pub version: u32,
    /// Immutable identifier supplied by trusted provisioning.
    pub machine_id: String,
    /// Canonical workspace room UUID.
    pub room: String,
    /// Native workspace u64 in decimal text form.
    pub workspace_id: String,
    /// Cloud host device public key; the host retains its secret.
    pub host_public_key: [u8; 32],
    /// Fresh one-time challenge nonce.
    pub nonce: [u8; 32],
    /// Expiry in native integer nanoseconds; never round-trip through JavaScript Number.
    pub expires_at_ns: u64,
    /// Existing pinned user root, or none before first enrollment.
    pub root_public_key: Option<[u8; 32]>,
    /// Existing stable host certificate, or the root-approved host certificate in a request.
    pub host_certificate: Option<DeviceCert>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Root authorization and independent proof of browser device-key custody.
pub struct EnrollmentRequest {
    /// Wire version; currently one.
    pub version: u32,
    /// Exact public challenge returned by the guest.
    pub challenge: Challenge,
    /// Root-signed certificate binding the browser-generated device key.
    pub browser_certificate: DeviceCert,
    /// Existing stable host certificate, or the root-approved host certificate in a request.
    pub host_certificate: DeviceCert,
    /// Root signature over the domain-separated canonical transcript.
    pub root_signature: Vec<u8>,
    /// Browser device signature over its separate canonical transcript.
    pub device_signature: Vec<u8>,
}

/// Narrow host delegation: directory ownership and hosting, without device admission.
pub fn host_capabilities() -> DeviceCapabilities {
    DeviceCapabilities {
        can_own_workspaces: true,
        can_admit_devices: false,
        can_act_as_host: true,
        can_read_all_workspaces: true,
        foreign: false,
        can_administer_devices: false,
    }
}

fn digest(request: &EnrollmentRequest, domain: &[u8]) -> Result<[u8; 32], String> {
    let bytes = crate::canonical::canonical_bytes(&(
        request.version,
        &request.challenge,
        &request.browser_certificate,
        &request.host_certificate,
    ))
    .map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&bytes);
    Ok(*hasher.finalize().as_bytes())
}

/// Sign authorization and device proof without serializing either secret.
///
/// # Errors
///
/// Returns a message when the request cannot be serialized for signing.
pub fn sign_request(
    request: &mut EnrollmentRequest,
    root: &SigningKey,
    device: &SigningKey,
) -> Result<(), String> {
    request.root_signature = root
        .sign(&digest(request, b"tilo-cloud-enrollment-root-v1\0")?)
        .to_bytes()
        .to_vec();
    request.device_signature = device
        .sign(&digest(request, b"tilo-cloud-enrollment-device-v1\0")?)
        .to_bytes()
        .to_vec();
    Ok(())
}

/// Verify the exact issued challenge, root authorization, and device custody.
///
/// # Errors
///
/// Returns a message when the version, challenge or expiry does not match,
/// or when either signature does not verify.
pub fn verify_request(
    request: &EnrollmentRequest,
    expected: &Challenge,
    now: u64,
) -> Result<(), String> {
    if request.version != 1
        || expected.version != 1
        || &request.challenge != expected
        || now >= expected.expires_at_ns
    {
        return Err("invalid or expired enrollment challenge".into());
    }
    let root = request.browser_certificate.user_public_key;
    if expected
        .host_certificate
        .as_ref()
        .is_some_and(|certificate| certificate != &request.host_certificate)
        || request.host_certificate.expires_at_ns.is_some()
    {
        return Err("host certificate must remain stable".into());
    }
    if expected.root_public_key.is_some_and(|key| key != root)
        || request.host_certificate.user_public_key != root
        || request.host_certificate.device_public_key != expected.host_public_key
        || request.browser_certificate.device_public_key == expected.host_public_key
    {
        return Err("enrollment identity binding mismatch".into());
    }
    if request.host_certificate.capabilities != host_capabilities()
        || request.browser_certificate.capabilities != DeviceCapabilities::member()
    {
        return Err("invalid enrollment capabilities".into());
    }
    for certificate in [&request.host_certificate, &request.browser_certificate] {
        verify_device_cert_at(certificate, now).map_err(|error| error.to_string())?;
        if certificate.issued_at_ns > now {
            return Err("certificate issued in future".into());
        }
    }
    for (key, signature, domain) in [
        (
            root,
            &request.root_signature,
            b"tilo-cloud-enrollment-root-v1\0".as_slice(),
        ),
        (
            request.browser_certificate.device_public_key,
            &request.device_signature,
            b"tilo-cloud-enrollment-device-v1\0".as_slice(),
        ),
    ] {
        let verifying_key = VerifyingKey::from_bytes(&key).map_err(|error| error.to_string())?;
        let signature = Signature::from_slice(signature).map_err(|error| error.to_string())?;
        verifying_key
            .verify(&digest(request, domain)?, &signature)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
