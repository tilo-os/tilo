//! Applications on the action door under their own identity.
//!
//! An application declares in its signed manifest what it may do on the
//! door beyond its own actions ([`AppActionAccess`]): read the catalogue,
//! invoke other apps' actions. The declaration is a ceiling, never a grant:
//! invoking another app's action still needs the workspace to grant it to
//! the app, exactly as it grants a collaborator.
//!
//! When an app reaches for an action it holds no grant for, the person who
//! admitted its connection is asked first — on a sheet the host draws
//! ([`AppActionConsentPrompter`]) — instead of refusing silently. The
//! answer becomes an ordinary capability in the workspace's records,
//! granted by that person and recorded by the host
//! ([`AppActionGrants`](crate::services::AppActionGrants)). Nobody present
//! means nobody to ask: the request is refused without a sheet.

use std::sync::Arc;

use tilo_action_policy::Lifetime;
use tokio::sync::oneshot;

/// What an application declared it may do on the door under its own
/// identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppActionAccess {
    /// Read the catalogue of actions the person using the app is offered.
    pub query: bool,
    /// Invoke actions other apps publish, as this app.
    pub invoke: bool,
}

/// Where declared access is read from, by package id.
pub type AppActionAccessResolver = Arc<dyn Fn(&str) -> AppActionAccess + Send + Sync>;

/// A resolver under which no application declared anything.
#[must_use]
pub fn nothing_declared() -> AppActionAccessResolver {
    Arc::new(|_| AppActionAccess::default())
}

/// What the person is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppActionConsentRequest {
    /// Names this one question; the sheet's handle.
    pub handle: String,
    /// The workspace the grant would be recorded in.
    pub room: String,
    /// Who admitted the asking application's connection, and would grant.
    pub person: String,
    /// The application asking.
    pub app_id: String,
    /// The application whose action it wants to use.
    pub target_app: String,
    /// The resource of `target_app` that publishes the action, as a grant
    /// scopes it.
    pub resource: String,
    /// The action's id on `resource`, as a grant scopes it.
    pub action: String,
    pub resource_label: String,
    pub action_label: String,
    pub risk: tilo_actions::RiskLevel,
    /// Whether each use still asks for confirmation, whatever is granted.
    pub needs_confirmation: bool,
    /// The person confirms this one invocation, which `app_id` (an agent or
    /// an application) makes for them, rather than granting anything.
    pub confirming: bool,
    /// Exact external disclosure, decoded by the host rather than supplied as a label.
    pub moderation: Option<ModerationConsentPreview>,
    /// Where a confirmation may also be allowed from now on, by the name the
    /// person knows it by: the most specific place the invocation runs in,
    /// such as the issue an agent works on. Confirming, the person may then
    /// answer [`AppActionConsent::Always`] for it, which grants the action
    /// there.
    pub scope: Option<String>,
    /// The invocation as the person reviews it, field by field, where they
    /// confirm exact arguments: what is admitted is what these render.
    pub draft: Option<tilo_actions::Draft>,
    /// The change the step makes, before and after, where it previews it
    /// ([`crate::services::Preview`]): the person decides on the difference,
    /// and what they confirm is what it shows.
    pub preview: Option<crate::services::Preview>,
}

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
    /// Decode the nested operation string in the original invocation. The signing
    /// identity is separate: no field in the operation may select it.
    pub(crate) fn from_argument(
        action: &str,
        argument: &tilo_actions::remote::InvokeArgument,
        signing_root: &[u8; 32],
    ) -> Option<Self> {
        use registry_types::ModerationMutation;
        let slot = argument.to_slot()?;
        let json = slot
            .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
            .unwrap_or(&slot);
        let arguments: serde_json::Value = serde_json::from_str(json).ok()?;
        let operation: registry_types::ExternalModerationOperation =
            serde_json::from_str(arguments.get("operation")?.as_str()?).ok()?;
        runtime_collab::external_moderation::validate_action(action, json, &operation).ok()?;
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
            _ => return None,
        };
        Some(Self {
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

/// The person's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppActionConsent {
    /// This one call.
    Once,
    /// Until this session ends.
    Session,
    /// In this workspace until revoked.
    Always,
    Deny,
}

impl AppActionConsent {
    /// The allowing answers, in the order the sheet offers them: the
    /// narrowest first, and selected by default.
    pub const ALLOWING: [Self; 3] = [Self::Once, Self::Session, Self::Always];

    /// How long the grant this answer records lives; `None` records
    /// nothing.
    #[must_use]
    pub fn grant_lifetime(self) -> Option<Lifetime> {
        match self {
            Self::Once => Some(Lifetime::Once),
            Self::Session => Some(Lifetime::Session(
                tilo_desktop_model::session_id().to_owned(),
            )),
            Self::Always => Some(Lifetime::UntilRevoked),
            Self::Deny => None,
        }
    }

    /// How long the grant lives, as the workspace's history records it:
    /// `once`, `session` or `workspace`.
    #[must_use]
    pub const fn lifetime(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Always | Self::Deny => "workspace",
        }
    }
}

/// Whoever can answer for the person using an application.
pub trait AppActionConsentPrompter: Send + Sync {
    /// Put `request` to the person. `None` when nobody is there to answer,
    /// which refuses the request without asking.
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionConsent>>;

    /// Tell the person about `request` without interrupting them, as an
    /// ordinary notice; when they choose to answer it, `answer` is handed
    /// what they answer on the sheet. `false` when nobody here is told.
    fn notify(&self, request: AppActionConsentRequest, answer: AnswerLater) -> bool {
        let _ = (request, answer);
        false
    }

    /// Take back the notice of the request `handle`: answered or gone.
    fn withdraw(&self, handle: &str) {
        let _ = handle;
    }
}

/// What a notified request does with the person's answer on the sheet.
pub type AnswerLater = Box<dyn FnOnce(oneshot::Receiver<AppActionConsent>) + Send>;

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

    #[test]
    fn moderation_preview_decodes_report_and_appeal_and_uses_admitted_identity() {
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

    #[test]
    fn moderation_preview_refuses_missing_malformed_and_mismatched_operations() {
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
                    .is_none()
            );
        }
        let slot = moderation_argument(false).to_slot().unwrap();
        assert!(
            ModerationConsentPreview::from_argument(
                "submit-report",
                &InvokeArgument::Text(slot),
                &[2; 32]
            )
            .is_some()
        );
    }

    #[test]
    fn each_answer_records_the_matching_lifetime() {
        assert_eq!(
            AppActionConsent::Once.grant_lifetime(),
            Some(Lifetime::Once)
        );
        assert_eq!(
            AppActionConsent::Session.grant_lifetime(),
            Some(Lifetime::Session(
                tilo_desktop_model::session_id().to_owned()
            ))
        );
        assert_eq!(
            AppActionConsent::Always.grant_lifetime(),
            Some(Lifetime::UntilRevoked)
        );
        assert_eq!(AppActionConsent::Deny.grant_lifetime(), None);
        assert_eq!(AppActionConsent::ALLOWING[0], AppActionConsent::Once);
    }
}
