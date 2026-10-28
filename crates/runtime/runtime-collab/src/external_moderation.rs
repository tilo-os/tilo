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

/// Check the Store action label against the operation's mutation.
/// Registry protocol labels are deliberately not Store action identifiers.
///
/// # Errors
/// Refuses unsupported actions, mutation mismatches or malformed operations.
pub fn validate_action_operation(
    action: &str,
    operation: &ExternalModerationOperation,
) -> Result<(), String> {
    use registry_types::ModerationMutation;
    if !matches!(
        (action, &operation.mutation),
        ("submit-report", ModerationMutation::FileReport { .. })
            | ("appeal", ModerationMutation::PublisherAppeal { .. })
    ) {
        return Err("external moderation differs from the admitted Store action".into());
    }
    operation.validate()
}

/// Compare the exact nested operation string in an admitted Store action.
/// Callers must strip any action-arguments transport prefix before validation.
///
/// # Errors
/// Refuses malformed or extra parameters, an empty app, unsupported actions,
/// or any difference from the frozen operation.
pub fn validate_action(
    action: &str,
    argument: &str,
    operation: &ExternalModerationOperation,
) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        app: String,
        operation: String,
    }
    validate_action_operation(action, operation)?;
    let arguments: Arguments = serde_json::from_str(argument)
        .map_err(|_| "invalid external moderation action arguments")?;
    if arguments.app.trim().is_empty() {
        return Err("external moderation requires an app".into());
    }
    let admitted: ExternalModerationOperation = serde_json::from_str(&arguments.operation)
        .map_err(|_| "invalid admitted external moderation operation")?;
    admitted.validate()?;
    if &admitted != operation {
        return Err("external moderation differs from the original admitted action".into());
    }
    Ok(())
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

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Complete an exact moderation operation under its admitted invocation.
    /// No signature or key material is returned to the caller.
    /// # Errors
    /// Refuses invalid custody, oversized envelopes or invalid case receipts.
    pub async fn external_moderation(
        &self,
        command: &Request,
        parent: [u8; 16],
    ) -> Result<registry_types::ModerationCase, runtime_portal::NotAvailable> {
        let failed =
            |message: String| runtime_portal::NotAvailable::Capnp(capnp::Error::failed(message));
        command.validate().map_err(failed)?;
        if parent == [0; 16] || uuid::Uuid::from_bytes(parent).to_string() != command.invocation {
            return Err(failed("external moderation invocation differs".into()));
        }
        let bytes = serde_json::to_vec(command).map_err(|e| failed(e.to_string()))?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(failed("external moderation request exceeds bound".into()));
        }
        let mut request = self.inner.local_file_clipboard_request();
        request.get().set_command_json(&bytes);
        request.get().set_on_behalf_of(&parent);
        let response = request.send().promise.await?;
        let bytes = response.get()?.get_reply_json()?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(failed("external moderation response exceeds bound".into()));
        }
        let case = serde_json::from_slice(bytes).map_err(|e| failed(e.to_string()))?;
        validate_receipt(command, &case).map_err(failed)?;
        Ok(case)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::{ModerationDestination, ModerationMutation};

    fn operation() -> ExternalModerationOperation {
        ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "https://registry.example".into(),
                owner_pubkey_hex: "ab".repeat(32),
            },
            request_id: "report-one".into(),
            issued_at_unix: 100,
            expires_at_unix: 200,
            mutation: ModerationMutation::FileReport {
                package_id: "org.example.app".into(),
                category: "misleading".into(),
                summary: "The package description is misleading".into(),
                evidence_digest_hex: "cd".repeat(32),
            },
        }
    }

    fn arguments(operation: &ExternalModerationOperation) -> String {
        serde_json::json!({
            "app": "org.example.app",
            "operation": serde_json::to_string(operation).unwrap(),
        })
        .to_string()
    }

    #[test]
    fn store_actions_bind_exact_nested_operations_not_protocol_labels() {
        let report = operation();
        let mut appeal = report.clone();
        appeal.mutation = ModerationMutation::PublisherAppeal {
            case_id: "case-one".into(),
            reason: "The report has been addressed".into(),
            evidence_digest_hex: None,
        };
        for (action, operation) in [("submit-report", report), ("appeal", appeal)] {
            let argument = arguments(&operation);
            validate_action(action, &argument, &operation).unwrap();
            for wrong in [
                operation.action(),
                "registry.publisher_reply",
                "report",
                "submit",
                "",
            ] {
                assert!(
                    validate_action(wrong, &argument, &operation).is_err(),
                    "{wrong}"
                );
            }
            let alternate = if action == "appeal" {
                "submit-report"
            } else {
                "appeal"
            };
            assert!(validate_action(alternate, &argument, &operation).is_err());
            for field in [
                "destination",
                "owner",
                "mutation",
                "request",
                "issued",
                "expires",
            ] {
                let mut changed = operation.clone();
                match field {
                    "destination" => changed.destination.base_url.push('/'),
                    "owner" => changed.destination.owner_pubkey_hex = "ef".repeat(32),
                    "mutation" => match &mut changed.mutation {
                        ModerationMutation::FileReport { summary, .. } => summary.push('!'),
                        ModerationMutation::PublisherAppeal { reason, .. } => reason.push('!'),
                        _ => unreachable!(),
                    },
                    "request" => changed.request_id.push('x'),
                    "issued" => changed.issued_at_unix += 1,
                    "expires" => changed.expires_at_unix += 1,
                    _ => unreachable!(),
                }
                assert!(
                    validate_action(action, &argument, &changed).is_err(),
                    "{field}"
                );
                assert!(
                    validate_action(action, &arguments(&changed), &operation).is_err(),
                    "{field}"
                );
            }
        }
    }

    #[test]
    fn store_action_arguments_reject_nonexact_and_malformed_payloads() {
        let operation = operation();
        let valid: serde_json::Value = serde_json::from_str(&arguments(&operation)).unwrap();
        let mut invalid = vec![
            serde_json::to_string(&operation).unwrap(),
            "null".into(),
            "{}".into(),
        ];
        for app in [
            serde_json::json!(""),
            serde_json::json!("  "),
            serde_json::json!(7),
        ] {
            let mut value = valid.clone();
            value["app"] = app;
            invalid.push(value.to_string());
        }
        for payload in [serde_json::json!(operation), serde_json::json!("not JSON")] {
            let mut value = valid.clone();
            value["operation"] = payload;
            invalid.push(value.to_string());
        }
        let mut extra = valid.clone();
        extra["unexpected"] = true.into();
        invalid.push(extra.to_string());
        let mut nested = serde_json::to_value(&operation).unwrap();
        nested["unexpected"] = true.into();
        let mut value = valid;
        value["operation"] = nested.to_string().into();
        invalid.push(value.to_string());
        invalid.push(format!(
            "{{\"app\":\"duplicate\",{}",
            &arguments(&operation)[1..]
        ));
        for argument in invalid {
            assert!(
                validate_action("submit-report", &argument, &operation).is_err(),
                "{argument}"
            );
        }
        let mut malformed = operation.clone();
        malformed.expires_at_unix = malformed.issued_at_unix;
        assert!(validate_action("submit-report", &arguments(&malformed), &malformed).is_err());
        let mut unsupported = operation;
        unsupported.mutation = ModerationMutation::PublisherReply {
            case_id: "case-one".into(),
            message: "Reply".into(),
            evidence_digest_hex: None,
        };
        assert!(validate_action("appeal", &arguments(&unsupported), &unsupported).is_err());
    }
}
