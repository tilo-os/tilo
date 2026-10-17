//! Exact operation envelope for the host's external moderation lane.

use registry_types::ExternalModerationOperation;
use serde::{Deserialize, Serialize};

/// Purpose-separated envelope identifier.
pub const PROTOCOL: &str = "tilo-external-moderation-v1";
/// Maximum encoded operation or case receipt.
pub const MAX_WIRE_BYTES: usize = 2 * 1024 * 1024;

/// Exact action parameters, tied to their framework invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: String,
    pub invocation: String,
    pub operation: ExternalModerationOperation,
}

impl Request {
    /// Validate the bounded envelope before giving it to the host.
    /// # Errors
    /// Refuses malformed invocation identifiers or operations.
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL
            || self.invocation.len() != 36
            || self.invocation.bytes().enumerate().any(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b != b'-'
                } else {
                    !b.is_ascii_hexdigit()
                }
            })
            || self.invocation == "00000000-0000-0000-0000-000000000000"
        {
            return Err("invalid external moderation envelope".into());
        }
        self.operation.validate()
    }
}

/// Check that a completed receipt belongs to the operation that was admitted.
/// This is not an owner-verified transparency proof of current status.
/// # Errors
/// Refuses a receipt for another case, package or operation outcome.
pub fn validate_receipt(
    request: &Request,
    case: &registry_types::ModerationCase,
) -> Result<(), String> {
    use registry_types::{ModerationCaseStatus, ModerationMutation};
    let valid = match &request.operation.mutation {
        ModerationMutation::FileReport { package_id, .. } => {
            case.package_id == *package_id
                && case.case_id
                    == registry_types::moderation_case_id(package_id, &request.operation.request_id)
                && case.status == ModerationCaseStatus::Open
        }
        ModerationMutation::PublisherAppeal { case_id, .. } => {
            case.case_id == *case_id && case.status == ModerationCaseStatus::Appealed
        }
        ModerationMutation::PublisherReply { case_id, .. } => case.case_id == *case_id,
        ModerationMutation::ReviewerDecision {
            case_id, decision, ..
        } => case.case_id == *case_id && case.status == decision.status(),
    };
    if valid && case.latest_sequence > 0 {
        Ok(())
    } else {
        Err("moderation receipt differs from admitted operation".into())
    }
}
