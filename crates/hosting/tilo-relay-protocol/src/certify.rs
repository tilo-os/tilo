//! Relay-mediated device certification on its own ALPN.
//!
//! A device without a certificate cannot pass the [`auth`](crate::auth)
//! handshake, so it asks for one here. Two kinds of connection share the
//! ALPN, told apart by the first frame:
//!
//! ```text
//! requester → host:  CertifyOpen::Request(CertificationRequest)
//! host → requester:  CertifyReply::Pending { pairing_code }
//!                    … later, on the same stream …
//! host → requester:  CertifyReply::Issued { cert, host_cert }
//!                  | CertifyReply::Denied { reason } | CertifyReply::Expired
//!
//! approver → host:   CertifyOpen::Approver(Hello)   then the usual
//!                    HelloAck / HelloAck2 mutual authentication
//! approver → host:   ApproverRequest::ListPending
//! host → approver:   ApproverReply::Pending(Vec<PendingCertification>)
//! approver → host:   ApproverRequest::Decide { request_digest, decision }
//! host → approver:   ApproverReply::Decided | ApproverReply::Refused { reason }
//! ```
//!
//! A request the host itself queued for a delegate device (an agent it
//! supervises, or a bot) carries a [`DelegateProposal`]: the label, colour, rooms, apps and expiry the person
//! asked for. `ApproverRequest::DelegateProposals` lists them, and the one
//! consent step is `ApproverDecision::IssueDelegate`, a capability-less
//! certificate together with the root-signed delegation of exactly that
//! proposal (or a narrower one). A bare `Issue` is refused for such a
//! request, so a delegate never exists without its delegation.
//!
//! An agent the host supervises asks its person before an action their
//! grants leave to them. The host queues the question for this one
//! invocation as a [`PendingConfirmation`]; an approver lists them
//! (`ApproverRequest::Confirmations`), shows each as the host's
//! confirmation sheet ([`PendingConfirmation::question`], Allow once or
//! Deny) and answers with `ApproverRequest::Answer`. An unanswered question
//! expires as a refusal.
//!
//! A member of a workspace the host serves pairs a new device the same way,
//! through any host they share: the new device posts its request to the
//! host's HTTPS `/certify` and shows its pairing code, and a page holding
//! the member's root reads and answers that member's pending requests with
//! [`MemberApproval`]s (`/certify/approve`), each signed by the root. The
//! host lists a member only their own requests and accepts a certificate
//! only for the exact request it answers.
//!
//! The host never signs a certificate. An approver is a device of the host's
//! user with `can_admit_devices`, i.e. one that holds root custody. The host
//! checks every issued certificate against the exact request and its
//! [`CertificationPolicy`](tilo_user_identity::CertificationPolicy) before
//! handing it on, and refuses revoked devices at request and at issuance.

use serde::{Deserialize, Serialize};
use tilo_user_identity::{CertificationRequest, DeviceCert};

use crate::auth::Hello;
use crate::delegation::{DelegateProposal, Delegation};

/// ALPN of the certification exchange. Frozen wire ABI.
pub const CERTIFY_ALPN: &[u8] = b"tilo-device-certify/1";
/// Largest certification frame either side accepts.
pub const MAX_CERTIFY_FRAME_BYTES: usize = 16 * 1024;
/// Requests one host holds at once; further requests are refused.
pub const MAX_PENDING_CERTIFICATIONS: usize = 32;

/// First frame on a certification stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CertifyOpen {
    /// An uncertified device asks to be certified.
    Request(CertificationRequest),
    /// A certified device opens an approver session.
    Approver(Hello),
}

/// What the host tells a waiting requester.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum CertifyReply {
    /// Queued for an approver. The same code is shown to the approver.
    Pending {
        /// Six-digit [`CertificationRequest::pairing_code`].
        pairing_code: String,
    },
    /// Issued and checked by the host.
    Issued {
        /// The root-signed certificate for the requesting device.
        cert: DeviceCert,
        /// The host's own certificate, which the device pins for later
        /// sessions.
        host_cert: DeviceCert,
    },
    /// An approver declined, or the host refused the request.
    Denied {
        /// Human-readable reason.
        reason: String,
    },
    /// No approver answered within the request lifetime.
    Expired,
}

/// One request awaiting an approver's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCertification {
    /// The exact signed request.
    pub request: CertificationRequest,
    /// Its [`CertificationRequest::digest`], used to answer it.
    pub request_digest: [u8; 32],
    /// The code the requester displays.
    pub pairing_code: String,
    /// When the host received the request, Unix-epoch nanoseconds.
    pub requested_at_ns: u64,
    /// The country the request came from (ISO 3166-1 alpha-2), when a
    /// proxy in front of the host that it trusts says so; the host looks
    /// nothing up itself.
    pub country: Option<String>,
}

/// An approver's answer to one request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum ApproverDecision {
    /// Root-signed certificate for the request.
    Issue(DeviceCert),
    /// Decline.
    Deny,
    /// A delegate's request (an agent or a bot): its capability-less
    /// certificate and the root-signed delegation of its proposal, approved
    /// together.
    IssueDelegate {
        /// Root-signed certificate for the delegate's device.
        cert: DeviceCert,
        /// Root-signed delegation; no wider than the proposal.
        delegation: Delegation,
    },
}

/// Domain label of a [`MemberApproval`] signature.
pub const MEMBER_APPROVAL_LABEL: &[u8] = b"tilo member approval v1\0";
/// How far a [`MemberApproval`]'s time may be from the host's clock.
pub const MEMBER_APPROVAL_SKEW_NS: u64 = 5 * 60 * 1_000_000_000;

/// An approver command from a member's root over HTTPS: only
/// [`ApproverRequest::ListPending`] (that member's own requests) and
/// [`ApproverRequest::Decide`] (one of them) are answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberApproval {
    /// The member's root, which signs.
    pub user_public_key: [u8; 32],
    /// When it was signed, Unix-epoch nanoseconds.
    pub issued_at_ns: u64,
    /// The command.
    pub request: ApproverRequest,
    /// Root Ed25519 signature over [`Self::signed_message`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

impl MemberApproval {
    /// What the root signs: the label, the root, the time and the command.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be encoded.
    pub fn signed_message(
        user_public_key: &[u8; 32],
        issued_at_ns: u64,
        request: &ApproverRequest,
    ) -> Result<Vec<u8>, postcard::Error> {
        let mut message = MEMBER_APPROVAL_LABEL.to_vec();
        message.extend_from_slice(user_public_key);
        message.extend_from_slice(&issued_at_ns.to_le_bytes());
        message.extend_from_slice(&postcard::to_allocvec(request)?);
        Ok(message)
    }

    /// Check the signature, and that it was signed within the allowed skew
    /// of `now_ns`.
    pub fn verify(&self, now_ns: u64) -> Result<(), &'static str> {
        if self.issued_at_ns.abs_diff(now_ns) > MEMBER_APPROVAL_SKEW_NS {
            return Err("the approval is too old or from the future");
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&self.user_public_key)
            .map_err(|_| "the approval names no valid root")?;
        let message = Self::signed_message(&self.user_public_key, self.issued_at_ns, &self.request)
            .map_err(|_| "the approval cannot be encoded")?;
        key.verify_strict(
            &message,
            &ed25519_dalek::Signature::from_bytes(&self.signature),
        )
        .map_err(|_| "the approval is not signed by its root")
    }
}

/// A pending request the host queued for a delegate, and what it proposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDelegate {
    /// [`PendingCertification::request_digest`] of the delegate's request.
    pub request_digest: [u8; 32],
    /// What the person asked the delegate to be allowed.
    pub proposal: DelegateProposal,
}

/// Approver command after mutual authentication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "a wire message, decoded once per request and never kept in bulk"
)]
pub enum ApproverRequest {
    /// Read every pending request.
    ListPending,
    /// Answer one pending request.
    Decide {
        /// [`PendingCertification::request_digest`] of the request.
        request_digest: [u8; 32],
        decision: ApproverDecision,
    },
    DelegateProposals,
    /// Read every question an agent is waiting on.
    Confirmations,
    /// Answer one question.
    Answer {
        /// [`PendingConfirmation::id`] of the question.
        id: [u8; 32],
        /// The person's answer, for that one invocation.
        answer: ConfirmationAnswer,
    },
}

/// A question an agent on the host puts to its person: one invocation of
/// an action the person's grants leave to them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingConfirmation {
    /// Names this question, to answer it.
    pub id: [u8; 32],
    /// [`crate::delegation::Delegate::agent_id`] of the asking agent.
    pub agent_id: String,
    /// The agent's display name.
    pub agent_label: String,
    pub action_label: String,
    /// What it acts on, as the record that offers it says.
    pub resource_label: String,
    /// What the action risks, as `tilo_actions::RiskLevel` names it
    /// (`none`, `low`, `destructive`, `privileged`, `sensitive`), for the
    /// sheet to state. An approver reads an unknown name as the gravest.
    pub risk: String,
    /// The room the agent acts in.
    pub room: String,
    /// Unanswered by then, the question is a refusal; ns since the epoch.
    pub expires_at_ns: u64,
}

impl PendingConfirmation {
    /// The question as the confirmation sheet puts it.
    #[must_use]
    pub fn question(&self) -> String {
        format!(
            "{} wants to {}: {}, for you",
            self.agent_label, self.action_label, self.resource_label
        )
    }
}

/// The person's answer to a [`PendingConfirmation`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmationAnswer {
    /// Run this one invocation.
    AllowOnce,
    /// Do not run it.
    Deny,
}

/// Host answer to an [`ApproverRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApproverReply {
    /// Every pending request.
    Pending(Vec<PendingCertification>),
    /// The decision was delivered to the requester.
    Decided,
    /// The decision was not accepted.
    Refused {
        /// Human-readable reason.
        reason: String,
    },
    DelegateProposals(Vec<PendingDelegate>),
    /// Every question an agent is waiting on.
    Confirmations(Vec<PendingConfirmation>),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test this is the assertion")]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn approval(root: &SigningKey, issued_at_ns: u64, request: ApproverRequest) -> MemberApproval {
        let user_public_key = root.verifying_key().to_bytes();
        let message =
            MemberApproval::signed_message(&user_public_key, issued_at_ns, &request).unwrap();
        MemberApproval {
            user_public_key,
            issued_at_ns,
            request,
            signature: root.sign(&message).to_bytes(),
        }
    }

    #[test]
    fn a_member_approval_holds_only_for_its_root_command_and_time() {
        let root = SigningKey::from_bytes(&[7; 32]);
        let now = 1_000 * MEMBER_APPROVAL_SKEW_NS;
        let signed = approval(&root, now, ApproverRequest::ListPending);
        assert_eq!(signed.verify(now + 1), Ok(()));
        assert!(
            signed.verify(now + 2 * MEMBER_APPROVAL_SKEW_NS).is_err(),
            "too old"
        );
        let mut other = signed.clone();
        other.request = ApproverRequest::Decide {
            request_digest: [1; 32],
            decision: ApproverDecision::Deny,
        };
        assert!(
            other.verify(now).is_err(),
            "the signature covers the command"
        );
        let mut stranger = signed;
        stranger.user_public_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(stranger.verify(now).is_err(), "and the root");
    }
}
