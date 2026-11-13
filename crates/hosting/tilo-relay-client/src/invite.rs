//! Joining a workspace another host governs, from a page with no session
//! there: the device the page enters with.
//!
//! The joining person is not a user of that host: their page holds its own
//! user root and certifies the device itself ([`guest_certificate`]). The host
//! admits it through the workspace's governed roster as the invitation's own
//! grant admits it, never as one of the host user's devices; the redemption
//! is an entry on the device's session ([`tilo_relay_protocol::entering`]).

use crate::signer::DeviceSigner;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::error::LinkError;

/// The page's own certificate for its device, signed by the person's own
/// root `root`: a member device named `label`, with no expiry.
///
/// # Errors
///
/// Returns [`LinkError::Sign`] when the root does not sign, or signs a
/// certificate that does not verify.
pub async fn guest_certificate(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
    issued_at_ns: u64,
) -> Result<DeviceCert, LinkError> {
    let label = tilo_user_identity::display_name(label)
        .map_err(|error| LinkError::Sign(format!("the device needs a name: {error}")))?;
    let mut cert = DeviceCert {
        user_public_key: root.device_public_key(),
        device_public_key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert_async(&mut cert, root)
        .await
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    Ok(cert)
}
