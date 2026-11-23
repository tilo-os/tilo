//! The certificates a person's root issues: answering a device's pairing
//! request through the server with a browser-policy certificate, and
//! certifying a device whose key the root's holder already has.

mod held_root;
pub use held_root::{
    GrantAsk, IssuedRole, SignInAsk, SignedIn, answer_sign_in, certify_with_root,
    issue_agent_grant, issue_device_cert, issue_sign_in_cert,
};

use crate::{CertifyError, DeviceSigner, MemberServer, decide_pairing, pending_pairings};
use tilo_user_identity::{
    CertificationError, CertificationPolicy, CertificationRequest, DeviceCert, RevocationList,
    issue_device_cert_async, verify_issued_cert,
};

/// Answer the named pending request at `server`. When approved, the person's
/// root certifies the device in that request, and returns its certificate for
/// the approver's records.
///
/// # Errors
///
/// The request no longer waits, could not be certified, or the server could
/// not be reached or refused the answer.
pub async fn answer_pairing(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
    request_digest: [u8; 32],
    approved: bool,
) -> Result<Option<DeviceCert>, CertifyError> {
    let issued = if approved {
        let pending = pending_pairings(server, root).await?;
        let Some(found) = pending
            .iter()
            .find(|entry| entry.request_digest == request_digest)
        else {
            return Err(CertifyError::Denied("that device no longer waits".into()));
        };
        Some(
            issue_browser_certificate(&found.request, root)
                .await
                .map_err(|error| CertifyError::Request(error.to_string()))?,
        )
    } else {
        None
    };
    decide_pairing(server, root, request_digest, issued.clone()).await?;
    Ok(issued)
}

/// Sign the exact request under the browser policy, whether the held root
/// signs immediately or answers asynchronously through `WebCrypto`.
async fn issue_browser_certificate(
    request: &CertificationRequest,
    root: &dyn DeviceSigner,
) -> Result<DeviceCert, CertificationError> {
    let policy = CertificationPolicy::browser();
    let revocations = RevocationList::new();
    let now = tilo_clock::unix_now_ns();
    let cert = issue_device_cert_async(request, root, &policy, &revocations, now).await?;
    verify_issued_cert(request, &cert, &policy, &revocations, now)?;
    Ok(cert)
}
