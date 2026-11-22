//! Enrollment through a server for a computer or browser that needs a member
//! certificate. The device signs its request with the key it already holds.

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "WebCrypto signs through JavaScript on the page's thread"
    )
)]

use tilo_server_client::{
    CERTIFICATION_WAIT_NS, CertificationOutcome, CertifyError, DeviceSigner, ServerAddress,
    request_certification_at,
};
use tilo_user_identity::{
    CERTIFICATION_CLOCK_ALLOWANCE_NS, CERTIFICATION_SCHEMA_VERSION, CertificationRequest,
    DeviceCapabilities, DeviceCert,
};

/// Ask the server at `server` to certify the device behind `signer` as one of
/// `user_public_key`'s devices, labeled `label`, answering the pairing invite
/// `invite` ([0; 16] when none). `on_pending` receives the pairing code to
/// show while the person's other device answers. Resolves to this device's
/// certificate, the server's to pin, and the name the server gave the device (the
/// one asked for, or the next free numbered one).
///
/// # Errors
///
/// The server was not reached or refused, nobody answered in time
/// ([`CertifyError::Expired`]), or what it issued is not a certificate for
/// this device.
pub async fn certify(
    server: &ServerAddress,
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<(DeviceCert, DeviceCert, String), CertifyError> {
    let request = signed_request(user_public_key, label, invite, signer).await?;
    match request_certification_at(server, request, on_pending).await? {
        CertificationOutcome::Issued {
            cert,
            server_cert,
            label,
        } => Ok((cert, server_cert, label)),
        CertificationOutcome::Denied(reason) => Err(CertifyError::Denied(reason)),
        CertificationOutcome::Expired => Err(CertifyError::Expired),
    }
}

/// The request a device without a certificate signs.
///
/// It is dated [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] before this device's
/// clock. A server from before servers allowed the clocks of a pairing to differ
/// checks the date exactly, and refused a request dated now whenever its own
/// clock ran a little behind this device's; such a server stays in use while
/// its pages move on. The wait for an approver is counted from now, as
/// before, so the request lasts that much longer than the wait, well inside
/// the longest a request may. With a server that has the allowance, a device
/// up to twice that far ahead still pairs; the expiry is exact everywhere.
async fn signed_request(
    user_public_key: [u8; 32],
    label: &str,
    invite: [u8; 16],
    signer: &dyn DeviceSigner,
) -> Result<CertificationRequest, CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let unsigned = CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key,
        device_public_key: signer.device_public_key(),
        device_label: label.to_owned(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite,
        issued_at_ns: now.saturating_sub(CERTIFICATION_CLOCK_ALLOWANCE_NS),
        expires_at_ns: now.saturating_add(CERTIFICATION_WAIT_NS),
        signature: [0; 64],
    };
    let digest = unsigned
        .signing_digest()
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    unsigned
        .with_signature(signature)
        .map_err(|error| CertifyError::Request(error.to_string()))
}
