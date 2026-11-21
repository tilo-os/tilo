//! A device already signed in answers the browser tab that showed a code.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_server_client::login_code::{self, CodeRefusal};
use tilo_server_client::{DeviceSigner, HttpClient};
use tilo_server_protocol::login_code::CODE_LIFETIME_MS;
use tilo_user_identity::{
    CertificationError, CertificationRequest, DeviceCert, RevocationList,
    issue_device_cert_as_device,
};

use super::issue_browser_certificate;

/// Where approval by code stopped, for the caller to present.
#[derive(Debug)]
pub enum VouchError {
    /// The certificate's wire encoding could not be produced.
    Encode(postcard::Error),
    /// The claim failed or the server refused it.
    Claim(CodeRefusal),
    /// The server stopped the sign-in while the browser request was expected.
    Waiting(CodeRefusal),
    /// The browser did not ask before the claim expired.
    Expired,
    /// The server gave no readable certification request.
    GarbledRequest,
    /// The request is for a different person than the one the claim pins.
    DifferentPerson,
    /// The root or the claiming device could not certify the request.
    Issue(CertificationError),
    /// The signed answer could not be delivered or was refused.
    Answer(CodeRefusal),
}

/// Claim the resolved code with the held device's key and certificate. Answer
/// only a request for the person pinned by the claim, using `root` if explicitly
/// held, or the claiming root-signed device itself. `on_claimed` runs after
/// the server accepts the claim, before waiting for the browser.
///
/// Dropping this future stops waiting and signing. The rendezvous is scoped
/// to the server's code and expires there independently.
///
/// # Errors
///
/// The claim or answer failed, or the exact request could not be certified
/// before the claim expired.
pub async fn vouch_with_code(
    server: &dyn HttpClient,
    report: &str,
    device_key: &SigningKey,
    device_cert: &DeviceCert,
    root: Option<&dyn DeviceSigner>,
    on_claimed: impl FnOnce(),
) -> Result<(), VouchError> {
    let claim_cert = postcard::to_allocvec(device_cert).map_err(VouchError::Encode)?;
    let claimed = login_code::claim(
        server,
        report,
        hex::encode(claim_cert),
        device_key,
        tilo_clock::unix_now_ns(),
    )
    .await
    .map_err(VouchError::Claim)?;
    on_claimed();

    let remaining_ms = claimed
        .expires_at_ms
        .saturating_sub(tilo_clock::unix_now_ms())
        .min(CODE_LIFETIME_MS);
    if remaining_ms == 0 {
        return Err(VouchError::Expired);
    }
    let timeout = Duration::from_millis(remaining_ms);
    let request = tokio::time::timeout(timeout, wait_for_request(server, report))
        .await
        .map_err(|_| VouchError::Expired)??;
    if tilo_clock::unix_now_ms() >= claimed.expires_at_ms {
        return Err(VouchError::Expired);
    }

    let request: CertificationRequest = hex::decode(request)
        .ok()
        .and_then(|bytes| postcard::from_bytes(&bytes).ok())
        .ok_or(VouchError::GarbledRequest)?;
    if request.user_public_key != device_cert.user_public_key {
        return Err(VouchError::DifferentPerson);
    }
    let cert = match root {
        Some(root) => issue_browser_certificate(&request, root).await,
        None => issue_device_cert_as_device(
            &request,
            device_key,
            device_cert,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        ),
    }
    .map_err(VouchError::Issue)?;
    let answer = postcard::to_allocvec(&cert).map_err(VouchError::Encode)?;
    login_code::certify(server, report, hex::encode(answer))
        .await
        .map_err(VouchError::Answer)
}

async fn wait_for_request(server: &dyn HttpClient, report: &str) -> Result<String, VouchError> {
    loop {
        match login_code::waiting(server, report).await {
            Ok(Some(request)) => return Ok(request),
            Ok(None) => {}
            Err(refusal) if refusal.code() == "unreachable" => {
                tracing::debug!(%refusal, "asking for the tab's request");
            }
            Err(refusal) => return Err(VouchError::Waiting(refusal)),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
