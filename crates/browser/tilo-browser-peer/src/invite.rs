//! Joining a room someone else's host governs, from a page with no session
//! there (`/join`; see [`tilo_relay_protocol::rooms`]).
//!
//! The joining person is not a user of that host: their page holds its own
//! user root beside its device key and certifies the device itself
//! ([`guest_certificate`]). The host admits the device through the room's
//! governed roster once the room's owner admits the request, never as one
//! of the host user's devices.

use tilo_relay_client::DeviceSigner;
use tilo_relay_protocol::rooms::{
    JoinChallengeView, JoinReply, JoinStep, MAX_JOIN_NAME_CHARS, join_proof_digest,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, device_cert_signing_digest};

use crate::carrier::{self, HttpClient, LinkError};

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
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_JOIN_NAME_CHARS {
        return Err(LinkError::Sign("the device needs a name".into()));
    }
    let mut cert = DeviceCert {
        user_public_key: root.device_public_key(),
        device_public_key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let digest =
        device_cert_signing_digest(&cert).map_err(|error| LinkError::Sign(error.to_string()))?;
    cert.signature = root
        .sign(digest.to_vec())
        .await
        .map_err(|error| LinkError::Sign(error.0))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| LinkError::Sign(error.to_string()))?;
    Ok(cert)
}

/// One `/join` step.
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached or answers badly.
pub async fn join(client: &dyn HttpClient, step: &JoinStep) -> Result<JoinReply, LinkError> {
    carrier::join(client, step).await
}

/// Ask to join with `invitation` as the device `cert` signed by `signer`,
/// named `name` to the room's owner: redeem it, check the host's challenge
/// names exactly this invitation, device and request, and answer it.
/// Resolves to [`JoinReply::Waiting`] once the request waits for the owner,
/// or to the host's refusal.
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached, answers badly, or
/// sends a challenge for anything else.
pub async fn ask_to_join(
    client: &dyn HttpClient,
    invitation: &[u8],
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    request_id: [u8; 32],
    name: &str,
) -> Result<JoinReply, LinkError> {
    if signer.device_public_key() != cert.device_public_key {
        return Err(LinkError::Sign(
            "the signer is not the certified device".into(),
        ));
    }
    let asked = join(
        client,
        &JoinStep::Ask {
            invitation: invitation.to_vec(),
            device_cert: cert.clone(),
            request_id,
        },
    )
    .await?;
    let challenge = match asked {
        JoinReply::Challenge { challenge } => challenge,
        other => return Ok(other),
    };
    let view: JoinChallengeView = postcard::from_bytes(&challenge)
        .map_err(|error| LinkError::Malformed(error.to_string()))?;
    if view.signed_invitation != invitation
        || view.user_public_key != cert.user_public_key
        || view.device_public_key != cert.device_public_key
        || view.request_id != request_id
        || postcard::to_allocvec(&view).ok().as_deref() != Some(challenge.as_slice())
    {
        return Err(LinkError::Malformed(
            "the host's join challenge names another request".into(),
        ));
    }
    let signature = signer
        .sign(join_proof_digest(&challenge).to_vec())
        .await
        .map_err(|error| LinkError::Sign(error.0))?;
    join(
        client,
        &JoinStep::Prove {
            challenge,
            signature,
            name: name.to_owned(),
        },
    )
    .await
}
