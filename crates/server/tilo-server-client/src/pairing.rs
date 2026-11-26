//! What a person's root gives their devices, and the pairing ceremony their
//! server carries between them: a new device asks to be certified
//! ([`certify`](crate::certify)), a root-holding device lists the waiting
//! requests ([`pending_pairings`]) and sends an answer ([`decide_pairing`]).
//!
//! Only the root signs: the server carries and checks, and hands nothing on
//! that a root did not sign for exactly that request.

#[cfg(feature = "dial")]
use tilo_server_protocol::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, MemberApproval, PendingDelegate,
};
use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry_async};
#[cfg(any(test, feature = "dial"))]
use tilo_user_identity::{RevocationList, issue_device_cert_async};

#[cfg(feature = "dial")]
use crate::certify::MemberServer;
use crate::error::CertifyError;
use crate::signer::DeviceSigner;

/// Have `root` revoke `device_public_key` as of `at_ns`, for `reason`: the
/// entry every server and device refuses that key from then on with, while what
/// it signed before stays valid.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, when signing fails, or
/// when the entry does not verify.
pub async fn revoke_device(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    at_ns: u64,
    reason: RevocationReason,
) -> Result<RevocationEntry, CertifyError> {
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "a root revokes the keys it made, never itself".into(),
        ));
    }
    let mut entry = RevocationEntry {
        user_public_key,
        revoked_device_public_key: device_public_key,
        revoked_at_ns: at_ns,
        reason,
        signature: [0; 64],
    };
    sign_revocation_entry_async(&mut entry, root)
        .await
        .map_err(|e| request(&e))?;
    tilo_user_identity::verify_revocation_entry(&entry).map_err(|e| request(&e))?;
    Ok(entry)
}

#[cfg(feature = "dial")]
pub(crate) async fn member_approval(
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<MemberApproval, CertifyError> {
    member_approval_as(root, None, request).await
}

/// [`member_approval`], signed by `signer`: the person's root when `device`
/// is `None`, else the device `device` certifies, which signs only what a
/// device may ([`MemberApproval::device`]).
#[cfg(feature = "dial")]
pub(crate) async fn member_approval_as(
    signer: &dyn DeviceSigner,
    device: Option<&tilo_user_identity::DeviceCert>,
    request: ApproverRequest,
) -> Result<MemberApproval, CertifyError> {
    let root = signer;
    let user_public_key =
        device.map_or_else(|| root.device_public_key(), |cert| cert.user_public_key);
    let issued_at_ns = tilo_clock::unix_now_ns();
    let digest = MemberApproval::signed_digest(&user_public_key, issued_at_ns, &request)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = root
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    Ok(MemberApproval {
        user_public_key,
        issued_at_ns,
        request,
        signature,
        device: device.cloned(),
    })
}

#[cfg(feature = "dial")]
async fn approve(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<ApproverReply, CertifyError> {
    let approval = member_approval(root, request).await?;
    match server.ask(&approval).await? {
        ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
        reply => Ok(reply),
    }
}

/// The requests of new devices waiting at `server` for `root` (a
/// member's root) to answer, each showing its pairing code.
///
/// # Errors
///
/// [`CertifyError::Transport`] when the server is not reached, or
/// [`CertifyError::Denied`] when it refuses.
#[cfg(feature = "dial")]
pub async fn pending_pairings(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
) -> Result<Vec<tilo_server_protocol::certify::PendingCertification>, CertifyError> {
    match approve(server, root, ApproverRequest::ListPending).await? {
        ApproverReply::Pending(pending) => Ok(pending),
        _ => Err(CertifyError::Denied(
            "the server answered something else".into(),
        )),
    }
}

/// The pending roster grants the root holder can review in this workspace.
///
/// # Errors
/// The server was not reached or the proposal or answer was refused.
#[cfg(feature = "dial")]
pub async fn pending_agent_grants(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
    workspace: collab_types::WorkspaceId,
) -> Result<Vec<PendingDelegate>, CertifyError> {
    match approve(server, root, ApproverRequest::DelegateProposals).await? {
        ApproverReply::DelegateProposals(mut proposals) => {
            proposals.retain(|entry| {
                entry.proposal.scope.workspaces.as_slice() == [workspace]
                    && entry.proposal.delegate.agent_id().is_some()
            });
            Ok(proposals)
        }
        _ => Err(CertifyError::Denied(
            "the server did not return pending agent grants".into(),
        )),
    }
}

/// Answer exactly the roster grant the root holder reviewed. A changed scope, certification
/// request, or workspace requires a fresh review.
///
/// # Errors
/// The server was not reached or the proposal or answer was refused.
#[cfg(feature = "dial")]
pub async fn answer_agent_grant(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
    workspace: collab_types::WorkspaceId,
    shown: &PendingDelegate,
    approved: bool,
) -> Result<(), CertifyError> {
    if !pending_agent_grants(server, root, workspace)
        .await?
        .contains(shown)
    {
        return Err(CertifyError::Denied(
            "that agent grant is no longer pending as shown".into(),
        ));
    }
    let decision = if approved {
        let request = pending_pairings(server, root)
            .await?
            .into_iter()
            .find(|entry| entry.request_digest == shown.request_digest)
            .ok_or_else(|| CertifyError::Denied("that agent no longer waits".into()))?
            .request;
        let invalid = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
        if request.digest().map_err(|error| invalid(&error))? != shown.request_digest {
            return Err(CertifyError::Denied(
                "the agent's certification request changed".into(),
            ));
        }
        let now = tilo_clock::unix_now_ns();
        let cert = issue_device_cert_async(
            &request,
            root,
            &tilo_user_identity::CertificationPolicy::agent(),
            &RevocationList::new(),
            now,
        )
        .await
        .map_err(|error| invalid(&error))?;
        let delegation = shown
            .proposal
            .sign_async(root, request.device_public_key, now)
            .await
            .map_err(|error| invalid(&error))?;
        ApproverDecision::IssueDelegate { cert, delegation }
    } else {
        ApproverDecision::Deny
    };
    match approve(
        server,
        root,
        ApproverRequest::Decide {
            request_digest: shown.request_digest,
            decision,
        },
    )
    .await?
    {
        ApproverReply::Decided => Ok(()),
        _ => Err(CertifyError::Denied(
            "the server did not settle the grant".into(),
        )),
    }
}

/// [`pending_pairings`] once they are other than `known`, the request digests
/// the caller holds: the server answers as a new device asks for `root` or one
/// is answered, or with what waits after
/// [`AWAIT_PENDING_NS`](tilo_server_protocol::certify::AWAIT_PENDING_NS).
///
/// # Errors
///
/// As [`pending_pairings`].
#[cfg(feature = "dial")]
pub async fn next_pairings(
    server: &impl MemberServer,
    root: &dyn DeviceSigner,
    known: Vec<[u8; 32]>,
) -> Result<Vec<tilo_server_protocol::certify::PendingCertification>, CertifyError> {
    match approve(server, root, ApproverRequest::AwaitPending { known }).await? {
        ApproverReply::Pending(pending) => Ok(pending),
        _ => Err(CertifyError::Denied(
            "the server answered something else".into(),
        )),
    }
}

/// Answer the pending request `request_digest` at `server`, as
/// `approver`: with `cert`, one the person's root already issued for exactly
/// that request, or turned away without one.
///
/// # Errors
///
/// The server was not reached or refused the answer.
#[cfg(feature = "dial")]
pub async fn decide_pairing(
    server: &impl MemberServer,
    approver: &dyn DeviceSigner,
    request_digest: [u8; 32],
    cert: Option<tilo_user_identity::DeviceCert>,
) -> Result<(), CertifyError> {
    let decision = match cert {
        Some(cert) => ApproverDecision::Issue(cert),
        None => ApproverDecision::Deny,
    };
    approve(
        server,
        approver,
        ApproverRequest::Decide {
            request_digest,
            decision,
        },
    )
    .await
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest,
        DeviceCapabilities, verify_device_cert,
    };

    #[tokio::test]
    #[cfg(feature = "dial")]
    async fn a_refused_signature_issues_no_approval() {
        struct Refusing;

        impl DeviceSigner for Refusing {
            fn device_public_key(&self) -> [u8; 32] {
                SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes()
            }

            fn sign(&self, _message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
                Box::pin(async { Err(collab_types::signer::SignError("locked".into())) })
            }
        }

        let approved = member_approval(&Refusing, ApproverRequest::ListPending).await;
        assert!(matches!(approved, Err(CertifyError::Request(_))));
    }

    #[tokio::test]
    async fn a_page_root_signs_the_same_certificate_and_revocation_as_a_held_key() {
        // A page's root answers only later; a held root answers at once. The
        // bytes either signs are the same, so a certificate a browser's root
        // made and one a machine's root made verify alike.
        let root = SigningKey::from_bytes(&[11; 32]);
        let device = SigningKey::from_bytes(&[12; 32]).verifying_key().to_bytes();
        let now = 1_700_000_000_000_000_000;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: [9; 16],
            invite: [0; 16],
            issued_at_ns: now,
            expires_at_ns: now + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(&SigningKey::from_bytes(&[12; 32]))
        .unwrap();
        let policy = CertificationPolicy::browser();
        let awaited =
            issue_device_cert_async(&request, &root, &policy, &RevocationList::new(), now)
                .await
                .unwrap();
        let at_once = tilo_user_identity::issue_device_cert(
            &request,
            &root,
            &policy,
            &RevocationList::new(),
            now,
        )
        .unwrap();
        assert_eq!(awaited, at_once);
        assert_eq!(verify_device_cert(&awaited), Ok(()));

        let mut later = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: now,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry_async(&mut later, &root)
            .await
            .unwrap();
        let mut held = later.clone();
        held.signature = [0; 64];
        tilo_user_identity::sign_revocation_entry(&mut held, &root).unwrap();
        assert_eq!(later, held);
        assert_eq!(tilo_user_identity::verify_revocation_entry(&later), Ok(()));
    }
}
