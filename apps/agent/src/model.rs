//! The model an agent's turn asks: the provider and model Settings (or the desktop that launched
//! the agent) chose, called through the desktop's model gateway, which holds the credential.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use agent_provider_config::protocol::{ModelRequest, ModelResponse};
use serde_json::Value;

use crate::providers::ProviderSelection;

/// One turn's model: `selection`'s profile and model through the model gateway, given up when
/// `stop` is set.
pub struct GatewayModel {
    pub selection: ProviderSelection,
    pub stop: Arc<AtomicBool>,
}

impl tilo_agent_turn::Model for GatewayModel {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(crate::i18n::t("agent-failure-timed-out"));
        }
        let stop = Arc::clone(&self.stop);
        tilo_model_client::complete_streaming_until(
            &self.selection.profile_id,
            &self.selection.model.id,
            request.clone(),
            left,
            &mut |_| {},
            &move || stop.load(Ordering::SeqCst),
        )
        .map_err(|error| {
            let error = error.model_error();
            tracing::info!(%error, "the agent's model did not answer");
            crate::i18n::t(person_key(error.kind))
        })
    }
}

/// The catalogue message for what a person reads when a model call failed: what happened in
/// their terms, and where there is a setting to look at, which one. The provider's own error is
/// the log's, never the reply's.
fn person_key(kind: agent_provider_config::protocol::ErrorKind) -> &'static str {
    use agent_provider_config::protocol::ErrorKind;
    match kind {
        ErrorKind::Unavailable => "agent-failure-unavailable",
        ErrorKind::Offline => "agent-failure-offline",
        ErrorKind::NoProvider => "agent-failure-no-provider",
        ErrorKind::Authentication => "agent-failure-authentication",
        ErrorKind::PermissionDenied => "agent-failure-permission-denied",
        ErrorKind::InvalidRequest => "agent-failure-invalid-request",
        ErrorKind::Conflict => "agent-failure-conflict",
        ErrorKind::InvalidResponse => "agent-failure-invalid-response",
        ErrorKind::Unsupported => "agent-failure-unsupported",
        ErrorKind::RateLimited => "agent-failure-rate-limited",
        ErrorKind::CreditsExhausted => "agent-failure-credits-exhausted",
        ErrorKind::ContextLimit => "agent-failure-context-limit",
        ErrorKind::Provider => "agent-failure-provider",
        ErrorKind::TimedOut => "agent-failure-timed-out",
        ErrorKind::Cancelled => "agent-failure-cancelled",
        ErrorKind::Truncated => "agent-failure-truncated",
        ErrorKind::Refused => "agent-failure-refused",
        ErrorKind::Transport => "agent-failure-transport",
    }
}

/// Why `selection`'s model cannot carry out a turn, naming it: it declares no tool calling, so
/// it could never use the workspace's tools. `None` for a model that calls tools.
#[must_use]
pub fn refuses_tasks(selection: &ProviderSelection) -> Option<String> {
    (!selection.model.tool_calling).then(|| {
        format!(
            "{} from {} cannot call tools, so it cannot carry out tasks. Choose a model with \
             tool calling for this agent or this task.",
            selection.model.display_label(),
            selection.profile_label
        )
    })
}

/// How many characters of conversation `selection`'s model reads at once, by what its descriptor
/// knows of its context, about four characters a token; a model that says nothing reads a
/// generous default.
#[must_use]
pub fn context_chars(selection: &ProviderSelection) -> usize {
    let model = &selection.model;
    let tokens = model
        .request_limits
        .as_ref()
        .and_then(|limits| limits.runtime_context_tokens)
        .or(model.context_window)
        .unwrap_or(128_000);
    let headroom = tokens.saturating_sub(model.max_output_tokens.unwrap_or(8_192));
    usize::try_from(headroom)
        .unwrap_or(usize::MAX)
        .saturating_mul(3)
}

/// Decode only the launcher's explicit nonsecret profile/model/protocol tuple,
/// and whether the model sees pictures. Endpoint, bearer credentials and
/// vault references never enter this contract.
pub fn managed_provider_selection(value: &Value) -> Result<ProviderSelection, String> {
    let object = value
        .as_object()
        .ok_or("managed provider must be an object")?;
    let flag = |name: &str, absent: bool| match object.get(name) {
        None => Ok(absent),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err("invalid managed provider field".to_string()),
    };
    let vision = flag("vision", false)?;
    // Said only of a model that calls no tools, so a run refuses it by name.
    let tool_calling = flag("tool_calling", true)?;
    let context_window = match object.get("context_window") {
        None => None,
        Some(Value::Number(tokens)) => Some(
            tokens
                .as_u64()
                .and_then(|tokens| u32::try_from(tokens).ok())
                .filter(|tokens| *tokens > 0)
                .ok_or("invalid managed provider field")?,
        ),
        Some(_) => return Err("invalid managed provider field".into()),
    };
    let optional = ["vision", "tool_calling", "context_window"]
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    if object.len() != 3 + optional
        || !object.keys().all(|key| {
            matches!(
                key.as_str(),
                "profile_id"
                    | "model_id"
                    | "protocol"
                    | "vision"
                    | "tool_calling"
                    | "context_window"
            )
        })
    {
        return Err("unsupported managed provider fields".into());
    }
    let field = |name: &str| -> Result<String, String> {
        let value = object
            .get(name)
            .and_then(Value::as_str)
            .ok_or("missing managed provider field")?;
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err("invalid managed provider field".into());
        }
        Ok(value.into())
    };
    let protocol = match field("protocol")?.as_str() {
        "anthropic" => agent_provider_config::ProviderProtocol::Anthropic,
        "open_ai_compatible" => agent_provider_config::ProviderProtocol::OpenAiCompatible,
        _ => return Err("unsupported managed provider protocol".into()),
    };
    let profile_id = field("profile_id")?;
    Ok(ProviderSelection {
        profile_label: profile_id.clone(),
        profile_id,
        endpoint: String::new(),
        protocol,
        model: agent_provider_config::ModelDescriptor {
            vision,
            tool_calling,
            context_window,
            ..agent_provider_config::ModelDescriptor::new(field("model_id")?)
        },
        credential: None,
    })
}

#[cfg(test)]
mod managed_tests {
    use super::*;
    #[test]
    fn managed_selection_requires_explicit_nonsecret_gateway_binding() {
        let value = serde_json::json!({"profile_id":"profile-1","model_id":"claude-1","protocol":"anthropic"});
        let selected = managed_provider_selection(&value).unwrap();
        assert_eq!(selected.profile_id, "profile-1");
        assert_eq!(selected.model.id, "claude-1");
        assert!(selected.credential.is_none());
        assert!(selected.endpoint.is_empty());
        assert!(!selected.model.vision);
        let mut seeing = value.clone();
        seeing["vision"] = serde_json::json!(true);
        assert!(managed_provider_selection(&seeing).unwrap().model.vision);
        seeing["vision"] = serde_json::json!("yes");
        assert!(managed_provider_selection(&seeing).is_err());
        for extra in ["api_key", "endpoint", "credential"] {
            let mut changed = value.clone();
            changed[extra] = serde_json::json!("disallowed");
            assert!(managed_provider_selection(&changed).is_err());
        }
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("model_id");
        assert!(managed_provider_selection(&missing).is_err());
    }

    /// The desktop says when a model calls no tools and how much it reads at
    /// once; a run reads both off the selection it was handed.
    #[test]
    fn managed_selection_carries_tool_calling_and_context() {
        let value =
            serde_json::json!({"profile_id":"p","model_id":"m","protocol":"open_ai_compatible"});
        let plain = managed_provider_selection(&value).unwrap();
        assert!(
            plain.model.tool_calling,
            "a model calls tools unless said otherwise"
        );
        assert_eq!(plain.model.context_window, None);
        let mut said = value.clone();
        said["tool_calling"] = serde_json::json!(false);
        said["context_window"] = serde_json::json!(32_768);
        let said = managed_provider_selection(&said).unwrap();
        assert!(!said.model.tool_calling);
        assert_eq!(said.model.context_window, Some(32_768));
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!("big"),
        ] {
            let mut wrong = value.clone();
            wrong["context_window"] = bad;
            assert!(managed_provider_selection(&wrong).is_err());
        }
        let refusal = refuses_tasks(&said).expect("refused");
        assert!(refusal.contains("m from p cannot call tools"), "{refusal}");
    }
}
