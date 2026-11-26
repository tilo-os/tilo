//! The membership exchange a device proves itself a member of a workspace with, over
//! whatever carries it: a native device's iroh connection to the host's join service
//! ([`crate::enroll::prove_membership`]), or a page's WebTransport session to the same
//! service, which has no iroh endpoint of its own.

use std::future::Future;

use crate::join::{self, JoinTransportRequest, JoinTransportResponse, WorkspaceId};
use tilo_user_identity::{DeviceCert, EndpointDelegation};

/// How long the statement a membership request carries that its endpoint
/// speaks for its device lasts: long enough for a session and its retries,
/// short enough that a leaked endpoint key is bounded by the device that
/// signed for it.
const MEMBERSHIP_DELEGATION_LIFETIME_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// The workspace's current state for a device that is a member: the membership
/// exchange for `workspace_id`, challenged and proven with the device's key and
/// presenting `device`, the certificate it is a member with or the renewal of it.
/// `endpoint` is the key the host sees the requests come from, and `ask` carries
/// one request to the host's join service and brings its answer back. `request_id`
/// names this exchange. The answer is checked by whoever takes it in.
///
/// # Errors
/// The serving device cannot be reached, or refuses the proof.
pub async fn prove<F, Fut>(
    endpoint: [u8; 32],
    ask: F,
    request_id: [u8; 32],
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    signer: &dyn collab_types::signer::DeviceSigner,
) -> Result<join::JoinBootstrap, String>
where
    F: Fn(JoinTransportRequest) -> Fut,
    Fut: Future<Output = Result<JoinTransportResponse, String>>,
{
    let delegation = endpoint_delegation(endpoint, device, signer).await?;
    let (challenge, governance) = match ask(JoinTransportRequest::MembershipChallenge(
        join::MembershipChallengeRequest {
            workspace_id,
            device_cert: device.clone(),
            request_id,
            delegation,
        },
    ))
    .await?
    {
        JoinTransportResponse::MembershipChallenge(value) => (value.challenge, value.governance),
        JoinTransportResponse::Error { message, .. } => return Err(refused(&message)),
        _ => return Err("the host answered no membership challenge".into()),
    };
    // A further device of a member writes its own row; a device the
    // workspace lists already writes nothing.
    let admission = crate::own_admission::sign_further_device_row(
        &workspace_id.to_string(),
        &governance,
        device,
        signer,
        tilo_clock::wall(),
    )
    .await?;
    let proof = join::sign_membership_proof(challenge, signer).await?;
    match ask(JoinTransportRequest::MembershipComplete(
        join::MembershipCompleteRequest { proof, admission },
    ))
    .await?
    {
        JoinTransportResponse::MembershipComplete(bootstrap) => Ok(bootstrap),
        JoinTransportResponse::Error { message, .. } => Err(refused(&message)),
        _ => Err("the host answered no membership".into()),
    }
}

/// This device's statement that `endpoint` speaks for it while that is not its
/// own key: a device that keeps its device key apart from the transport key it
/// dials with asks the key it can sign with to vouch for the transport key.
pub(crate) async fn endpoint_delegation(
    endpoint: [u8; 32],
    device: &DeviceCert,
    signer: &dyn collab_types::signer::DeviceSigner,
) -> Result<Option<EndpointDelegation>, String> {
    if endpoint == device.device_public_key {
        return Ok(None);
    }
    let now = tilo_clock::unix_now_ns();
    let unsigned = EndpointDelegation::unsigned(
        device.device_public_key,
        endpoint,
        now,
        now.saturating_add(MEMBERSHIP_DELEGATION_LIFETIME_NS),
    );
    let digest = unsigned
        .signing_digest()
        .map_err(|error| error.to_string())?;
    // A page's key signs through its promise, so this is awaited rather than
    // taken from the turn.
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(unsigned.with_signature(signature)))
}

/// A host's refusal, in words fit to show: its control characters dropped and
/// its length bounded.
pub(crate) fn refused(message: &str) -> String {
    let message: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect();
    format!("the workspace's host refused: {message}")
}
