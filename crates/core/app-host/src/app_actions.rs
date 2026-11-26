//! Applications on the action api under their own identity.
//!
//! An application declares in its signed manifest what it may do on the
//! api beyond its own actions ([`AppActionAccess`]): read the catalogue,
//! invoke other apps' actions. The declaration is a ceiling, never a grant:
//! using another app's action still needs the person's own answer, which the
//! call waits for as its asked record. Approving it for good is a grant to the
//! app in the workspace ([`tilo_action_policy::Grantee::App`]).

use tilo_actions::remote::asked::Fact;
pub use tilo_broker_core::access::{
    AppActionAccess, AppActionAccessResolver, AppResourceKindsResolver, nothing_declared,
};

/// The Store moderation disclosure and the root identity the host will use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModerationConsentPreview {
    pub purpose: ModerationConsentPurpose,
    pub destination: String,
    pub owner_root: String,
    /// Derived from the admitted caller or the host's trusted device certificate.
    pub signing_root: String,
    /// Package for a report; case for an appeal.
    pub target: String,
    pub category: Option<String>,
    pub content: String,
    pub evidence_digest: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationConsentPurpose {
    Report,
    Appeal,
}

impl ModerationConsentPreview {
    /// What the person reads of this disclosure before they answer: what leaves the workspace,
    /// where to, and whose identity signs it.
    #[must_use]
    pub fn facts(&self) -> Vec<Fact> {
        let mut facts = vec![
            Fact::new(
                "purpose",
                match self.purpose {
                    ModerationConsentPurpose::Report => "report",
                    ModerationConsentPurpose::Appeal => "appeal",
                },
            ),
            Fact::new("destination", self.destination.clone()),
            Fact::new("registry", self.owner_root.clone()),
            Fact::new("signed by", self.signing_root.clone()),
            Fact::new("about", self.target.clone()),
        ];
        if let Some(category) = &self.category {
            facts.push(Fact::new("category", category.clone()));
        }
        facts.push(Fact::new("content", self.content.clone()));
        if let Some(evidence) = &self.evidence_digest {
            facts.push(Fact::new("evidence", evidence.clone()));
        }
        facts
    }

    /// Decode the nested operation string in the original invocation. The signing
    /// identity is separate: no field in the operation may select it.
    ///
    /// # Errors
    /// Why the invocation is refused: no argument, arguments or an
    /// `operation` that do not decode, or an operation other than the action's.
    pub(crate) fn from_argument(
        action: &str,
        argument: &tilo_actions::remote::InvokeArgument,
        signing_root: &[u8; 32],
    ) -> Result<Self, String> {
        use registry_types::ModerationMutation;
        let slot = argument
            .to_slot()
            .ok_or("Store disclosure has no arguments")?;
        let json = slot
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(&slot);
        let arguments: serde_json::Value = serde_json::from_str(json)
            .map_err(|error| format!("Store disclosure arguments are not JSON: {error}"))?;
        let operation = arguments
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or("Store disclosure arguments name no operation text")?;
        let operation: registry_types::ExternalModerationOperation =
            serde_json::from_str(operation)
                .map_err(|error| format!("Store disclosure operation is malformed: {error}"))?;
        runtime_collab::external_moderation::validate_action(action, json, &operation)?;
        let (purpose, target, category, content, evidence_digest) = match operation.mutation {
            ModerationMutation::FileReport {
                package_id,
                category,
                summary,
                evidence_digest_hex,
            } => (
                ModerationConsentPurpose::Report,
                package_id,
                Some(category),
                summary,
                Some(evidence_digest_hex),
            ),
            ModerationMutation::PublisherAppeal {
                case_id,
                reason,
                evidence_digest_hex,
            } => (
                ModerationConsentPurpose::Appeal,
                case_id,
                None,
                reason,
                evidence_digest_hex,
            ),
            _ => return Err("Store disclosure operation is neither a report nor an appeal".into()),
        };
        Ok(Self {
            purpose,
            destination: operation.destination.base_url,
            owner_root: operation.destination.owner_pubkey_hex,
            signing_root: hex::encode(signing_root),
            target,
            category,
            content,
            evidence_digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moderation_argument(appeal: bool) -> tilo_actions::remote::InvokeArgument {
        let mutation = if appeal {
            serde_json::json!({"kind": "publisher_appeal",
                "case_id": "case-one", "reason": "The problem has been fixed",
                "evidence_digest_hex": null
            })
        } else {
            serde_json::json!({"kind": "file_report",
                "package_id": "org.example.app", "category": "policy",
                "summary": "The package description is misleading",
                "evidence_digest_hex": "cd".repeat(32)
            })
        };
        let operation = serde_json::json!({
            "destination": {"base_url": "https://registry.example", "owner_pubkey_hex": "ab".repeat(32)},
            "request_id": "report-one", "issued_at_unix": 100, "expires_at_unix": 200,
            "mutation": mutation
        });
        tilo_actions::remote::InvokeArgument::Json(
            serde_json::json!({
                "app": "org.example.app", "operation": operation.to_string()
            })
            .as_object()
            .unwrap()
            .clone(),
        )
    }

    tilo_test_env::scenario! {
        fn a_moderation_preview_decodes_only_well_formed_operations;

        phase moderation_preview_decodes_report_and_appeal_and_uses_admitted_identity;
        {
            for (appeal, action, purpose, target, content) in [
                (
                    false,
                    "submit-report",
                    ModerationConsentPurpose::Report,
                    "org.example.app",
                    "The package description is misleading",
                ),
                (
                    true,
                    "appeal",
                    ModerationConsentPurpose::Appeal,
                    "case-one",
                    "The problem has been fixed",
                ),
            ] {
                let preview = ModerationConsentPreview::from_argument(
                    action,
                    &moderation_argument(appeal),
                    &[0x12; 32],
                )
                .unwrap();
                assert_eq!(preview.purpose, purpose);
                assert_eq!(preview.target, target);
                assert_eq!(preview.content, content);
                assert_eq!(preview.signing_root, "12".repeat(32));
                assert_eq!(preview.owner_root, "ab".repeat(32));
                assert_eq!(preview.destination, "https://registry.example");
                assert_eq!(preview.category.as_deref(), (!appeal).then_some("policy"));
                assert_eq!(preview.evidence_digest, (!appeal).then(|| "cd".repeat(32)));
            }
        }

        phase moderation_preview_refuses_missing_malformed_and_mismatched_operations;
        {
            use tilo_actions::remote::InvokeArgument;
            for argument in [
                InvokeArgument::None,
                InvokeArgument::Text("not json".into()),
                InvokeArgument::Json(
                    serde_json::json!({"operation": "{}"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                moderation_argument(true),
            ] {
                assert!(
                    ModerationConsentPreview::from_argument("submit-report", &argument, &[1; 32])
                        .is_err()
                );
            }
            let refused = ModerationConsentPreview::from_argument(
                "submit-report",
                &InvokeArgument::Json(
                    serde_json::json!({"app": "org.example.app", "operation": "{}"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                &[1; 32],
            )
            .unwrap_err();
            assert!(refused.contains("operation is malformed"), "{refused}");
            let slot = moderation_argument(false).to_slot().unwrap();
            assert!(
                ModerationConsentPreview::from_argument(
                    "submit-report",
                    &InvokeArgument::Text(slot),
                    &[2; 32]
                )
                .is_ok()
            );
        }
    }
}
