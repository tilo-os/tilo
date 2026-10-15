//! Trusted host binding, deliberately not serializable/deserializable.
//! Implementations come from launcher/session authentication, never RPC JSON.

#[derive(Clone, Copy)]
pub enum Caller {
    Agent,
    Chat,
    Photos,
    Settings,
}

/// A revocable authenticated caller. Settings identity alone grants no
/// credential-management permission. Implementations must consult current
/// session state rather than just checking the original token expiry.
pub trait Session: Send + Sync {
    fn caller(&self) -> Caller;
    fn may_manage_credentials(&self) -> bool {
        false
    }
    fn active(&self) -> bool;
}

#[cfg(any(unix, test))]
pub(crate) fn permitted(session: &dyn Session, request: &crate::Request) -> bool {
    if !session.active() {
        return false;
    }
    if matches!(session.caller(), Caller::Chat) {
        return chat_completion(request);
    }
    match request {
        crate::Request::SecretStatus { .. }
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
#[cfg(any(unix, test))]
pub(crate) fn chat_completion(request: &crate::Request) -> bool {
    use agent_provider_config::protocol::{Content, Role, ToolChoice};
    matches!(request, crate::Request::Complete { request, .. }
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

    fn completion() -> serde_json::Value {
        serde_json::json!({
            "verb": "complete", "profile_id": "profile", "model_id": "model",
            "timeout_ms": 1000,
            "request": {"messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}],
                "tools": [], "tool_choice": "none", "max_output_tokens": 16}
        })
    }

    #[tokio::test]
    async fn chat_cannot_acquire_agent_photos_or_settings_authority() {
        let chat = Authenticated(Caller::Chat, true, true);
        let request = serde_json::from_value(completion()).unwrap();
        assert!(permitted(&chat, &request));
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
                _ => "not authorized",
            };
            assert!(response.error.unwrap().contains(expected), "{value}");
            let response = gateway
                .execute(&chat, serde_json::from_value(value).unwrap(), &mut |_| true)
                .await;
            assert!(!response.ok);
            assert!(response.error.unwrap().contains("not authorized"));
        }
    }

    #[test]
    fn legacy_management_requires_live_settings_custody_permission() {
        for request in [
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
