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
//! A request the host itself queued for an agent it supervises carries an
//! [`AgentProposal`]: the label, colour, rooms, apps and expiry the person
//! asked for. `ApproverRequest::AgentProposals` lists them, and the one
//! consent step is `ApproverDecision::IssueAgent`, a capability-less
//! certificate together with the root-signed delegation of exactly that
//! proposal (or a narrower one). A bare `Issue` is refused for such a
//! request, so an agent never exists without its delegation.
//!
//! The host never signs a certificate. An approver is a device of the host's
//! user with `can_admit_devices`, i.e. one that holds root custody. The host
//! checks every issued certificate against the exact request and its
//! [`CertificationPolicy`](tilo_user_identity::CertificationPolicy) before
//! handing it on, and refuses revoked devices at request and at issuance.

use serde::{Deserialize, Serialize};
use tilo_user_identity::{CertificationRequest, DeviceCert};

use crate::agents::{AgentDelegation, AgentProposal};
use crate::auth::Hello;

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
    /// An agent request: its capability-less certificate and the root-signed
    /// delegation of its proposal, approved together.
    IssueAgent {
        /// Root-signed certificate for the agent's device.
        cert: DeviceCert,
        /// Root-signed delegation; no wider than the proposal.
        delegation: AgentDelegation,
    },
}

/// A pending request the host queued for an agent, and what it proposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingAgent {
    /// [`PendingCertification::request_digest`] of the agent's request.
    pub request_digest: [u8; 32],
    /// What the person asked the agent to be allowed.
    pub proposal: AgentProposal,
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
        /// The decision.
        decision: ApproverDecision,
    },
    /// Read the proposals of every pending agent request.
    AgentProposals,
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
    /// Every pending agent request's proposal.
    AgentProposals(Vec<PendingAgent>),
}
