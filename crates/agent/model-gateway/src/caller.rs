//! Trusted host binding, deliberately not serializable/deserializable.
//! Implementations come from launcher/session authentication, never RPC JSON.

#[derive(Clone, Copy)]
pub enum Caller {
    Agent,
    Chat,
    Photos,
    Settings,
}

/// The exact launcher identities admitted as provider control panels.
/// This is a role mapping, not an administration grant: sessions must still
/// hold the current credential-management permission.
pub fn provider_admin_app(app_id: &str) -> bool {
    matches!(app_id, "so.tilo.settings" | "so.tilo.model-providers")
}

/// A revocable authenticated caller. Settings identity alone grants no
/// provider-administration or credential-management permission. Implementations must consult current
/// session state rather than just checking the original token expiry.
pub trait Session: Send + Sync {
    fn caller(&self) -> Caller;
    fn may_manage_credentials(&self) -> bool {
        false
    }
    fn active(&self) -> bool;
}

#[cfg(any(all(unix, feature = "server"), test))]
pub(crate) fn permitted(session: &dyn Session, request: &crate::Request) -> bool {
    if !session.active() {
        return false;
    }
    if matches!(session.caller(), Caller::Chat) {
        return chat_completion(request);
    }
    match request {
        crate::Request::Complete { .. } | crate::Request::CompleteDefault { .. } => {
            matches!(session.caller(), Caller::Agent)
        }
        crate::Request::GetProviderConfig
        | crate::Request::SetProviderConfig { .. }
        | crate::Request::SecretStatus { .. }
        | crate::Request::CredentialStatus { .. }
        | crate::Request::MigrateApiKey { .. }
        | crate::Request::RemoveLegacyApiKey { .. }
        | crate::Request::SetApiKey { .. }
        | crate::Request::RemoveApiKey { .. } => {
            matches!(session.caller(), Caller::Settings) && session.may_manage_credentials()
        }
        _ => true, // The shared service additionally enforces app-specific verbs.
    }
}

/// Chat can generate conversation text, not execute tools or replay tool history.
/// Shared by launcher RPC and authenticated HTTP sessions.
#[cfg(any(all(unix, feature = "server"), test))]
pub(crate) fn chat_completion(request: &crate::Request) -> bool {
    use agent_provider_config::protocol::{Content, Role, ToolChoice};
    matches!(request, crate::Request::Complete { request, .. } | crate::Request::CompleteDefault { request, .. }
        if request.tools.is_empty()
            && request.tool_choice == ToolChoice::None
            && request.messages.iter().all(|message|
                message.role != Role::Tool
                    && message.content.iter().all(|content|
                        !matches!(content, Content::ToolCall(_) | Content::ToolResult(_)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Authenticated(Caller, bool, bool);
    impl Session for Authenticated {
        fn caller(&self) -> Caller {
            self.0
        }
        fn may_manage_credentials(&self) -> bool {
            self.1
        }
        fn active(&self) -> bool {
            self.2
        }
    }

    #[cfg(all(unix, feature = "server"))]
    fn completion() -> serde_json::Value {
        serde_json::json!({
            "verb": "complete", "profile_id": "profile", "model_id": "model",
            "timeout_ms": 1000,
            "request": {"messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}],
                "tools": [], "tool_choice": "none", "max_output_tokens": 16}
        })
    }

    #[cfg(all(unix, feature = "server"))]
    #[tokio::test]
    async fn chat_cannot_acquire_agent_photos_or_settings_authority() {
        let chat = Authenticated(Caller::Chat, true, true);
        let request = serde_json::from_value(completion()).unwrap();
        assert!(permitted(&chat, &request));
        let mut default = completion();
        default["verb"] = "complete_default".into();
        assert!(permitted(&chat, &serde_json::from_value(default).unwrap()));
        assert!(!permitted(
            &Authenticated(Caller::Chat, true, false),
            &request
        ));
        let mut forbidden = vec![
            serde_json::json!({"verb":"selection_configured", "profile_id":"p", "model_id":"m", "protocol":"open_ai_compat"}),
            serde_json::json!({"verb":"plan", "profile_id":"p", "model_id":"m", "body":{}, "timeout_ms":1}),
            serde_json::json!({"verb":"transform", "profile_id":"p", "model_id":"m", "instructions":"x", "input":"x", "max_output_tokens":1, "timeout_ms":1}),
            serde_json::json!({"verb":"analyze_image", "profile_id":"p", "model_id":"m", "image":{"media_type":"image/png", "data_base64":"AA==", "source_content_hash":"hash"}}),
            serde_json::json!({"verb":"trace", "trace":{"id":1,"turn":0}, "event":"plan_ready"}),
            serde_json::json!({"verb":"cluster_faces", "profile_id":"p", "model_id":"m", "images":[]}),
            serde_json::json!({"verb":"secret_status", "profile_id":"p"}),
            serde_json::json!({"verb":"credential_status", "profile_id":"p"}),
            serde_json::json!({"verb":"migrate_api_key", "profile_id":"p", "original_endpoint":"https://example.com"}),
            serde_json::json!({"verb":"remove_legacy_api_key", "profile_id":"p"}),
            serde_json::json!({"verb":"set_api_key", "profile_id":"p", "api_key":"x"}),
            serde_json::json!({"verb":"remove_api_key", "profile_id":"p"}),
        ];
        // Use the typed protocol enum rather than guessing its serialized name.
        forbidden[0]["protocol"] =
            serde_json::to_value(agent_provider_config::ProviderProtocol::OpenAiCompatible)
                .unwrap();
        for choice in ["auto", "required"] {
            let mut value = completion();
            value["request"]["tool_choice"] = choice.into();
            forbidden.push(value);
        }
        let mut value = completion();
        value["request"]["tools"] =
            serde_json::json!([{"name":"run", "description":"tool", "input_schema":{}}]);
        forbidden.push(value);
        for content in [
            serde_json::json!({"type":"tool_call", "id":"call", "name":"run", "input":{}}),
            serde_json::json!({"type":"tool_result", "call_id":"call", "content":"done", "is_error":false}),
        ] {
            let mut value = completion();
            value["request"]["messages"][0]["content"] = serde_json::json!([content]);
            forbidden.push(value);
        }
        let mut value = completion();
        value["request"]["messages"][0]["role"] = "tool".into();
        // Request JSON cannot replace the launcher/session identity.
        value["caller"] = "agent".into();
        value["app_id"] = "so.tilo.agent".into();
        forbidden.push(value);
        let default_forbidden: Vec<_> = forbidden
            .iter()
            .filter(|value| value["verb"] == "complete")
            .map(|value| {
                let mut value = value.clone();
                value["verb"] = "complete_default".into();
                value
            })
            .collect();
        forbidden.extend(default_forbidden);
        forbidden.push(serde_json::json!({"verb":"get_provider_config"}));
        forbidden.push(serde_json::json!({"verb":"set_provider_config", "expected_revision":"stale", "config":{}}));
        let http = crate::service::provider_client().unwrap();
        let gateway = crate::service::Gateway::new().unwrap();
        for value in forbidden {
            let request = serde_json::from_value(value.clone()).unwrap();
            assert!(!permitted(&chat, &request), "{value}");
            // Exercise both transport entry points; rejection precedes any
            // provider configuration, credential or network access.
            let response =
                crate::service::handle_streaming("so.tilo.chat", request, &http, &mut |_| true)
                    .await;
            assert!(!response.ok, "{value}");
            let expected = match value["verb"].as_str().unwrap() {
                "plan" => "only tilo-agent may plan",
                "transform" => "only tilo-agent may run transforms",
                "cluster_faces" => "only Photos may request fixed local face clustering",
                "analyze_image" => "only Photos may request fixed local image analysis",
                "complete" | "complete_default" | "get_provider_config" | "set_provider_config" => {
                    "Provider permission denied"
                }
                _ => "not authorized",
            };
            assert!(response.error.unwrap().contains(expected), "{value}");
            let response = gateway
                .execute(&chat, serde_json::from_value(value).unwrap(), &mut |_| true)
                .await;
            assert!(!response.ok);
            assert_eq!(
                response.model_error.unwrap().kind,
                agent_provider_config::protocol::ErrorKind::PermissionDenied
            );
            assert_eq!(
                response.error.as_deref(),
                Some("Provider permission denied")
            );
        }
    }

    #[test]
    fn provider_control_panel_identity_is_exact() {
        for app in ["so.tilo.settings", "so.tilo.model-providers"] {
            assert!(provider_admin_app(app));
        }
        for app in [
            "model-providers",
            "Model Providers",
            "so.tilo.model-providers.evil",
            "so.tilo.settings.evil",
            "so.tilo.chat",
            "so.tilo.agent",
            "*",
        ] {
            assert!(!provider_admin_app(app), "{app}");
        }
    }

    #[test]
    fn legacy_management_requires_live_settings_custody_permission() {
        for request in [
            crate::Request::GetProviderConfig,
            crate::Request::SetProviderConfig {
                expected_revision: "stale".into(),
                config: agent_provider_config::ProviderConfig::default(),
            },
            crate::Request::CredentialStatus {
                profile_id: "profile".into(),
            },
            crate::Request::MigrateApiKey {
                profile_id: "profile".into(),
                original_endpoint: "https://original.example".into(),
            },
            crate::Request::RemoveLegacyApiKey {
                profile_id: "profile".into(),
            },
        ] {
            assert!(permitted(
                &Authenticated(Caller::Settings, true, true),
                &request
            ));
            assert!(!permitted(
                &Authenticated(Caller::Settings, false, true),
                &request
            ));
            assert!(!permitted(
                &Authenticated(Caller::Settings, true, false),
                &request
            ));
            assert!(!permitted(
                &Authenticated(Caller::Agent, true, true),
                &request
            ));
            assert!(!permitted(
                &Authenticated(Caller::Photos, true, true),
                &request
            ));
        }
    }
}
