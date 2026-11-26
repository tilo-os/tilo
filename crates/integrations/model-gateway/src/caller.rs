//! What a launched app may ask of the gateway, by the identity its launcher
//! handed over: never by anything in the request JSON.

use tilo_app_id::apps;

/// The exact launcher identities admitted as provider control panels.
pub fn provider_admin_app(app_id: &str) -> bool {
    matches!(app_id, apps::SETTINGS)
}

/// Chat can generate conversation text, not execute tools or replay tool history.
#[cfg(feature = "server")]
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

    #[cfg(feature = "server")]
    fn completion() -> serde_json::Value {
        serde_json::json!({
            "verb": "complete", "profile_id": "profile", "model_id": "model",
            "timeout_ms": 1000,
            "request": {"messages": [{"role": "user", "content": [{"type": "text", "text": "Hello"}]}],
                "tools": [], "tool_choice": "none", "max_output_tokens": 16}
        })
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn chat_cannot_acquire_agent_photos_or_settings_authority() {
        let request = serde_json::from_value(completion()).unwrap();
        assert!(chat_completion(&request));
        let mut default = completion();
        default["verb"] = "complete_default".into();
        assert!(chat_completion(&serde_json::from_value(default).unwrap()));
        let mut forbidden = vec![
            serde_json::json!({"verb":"analyze_image", "profile_id":"p", "model_id":"m", "image":{"media_type":"image/png", "data_base64":"AA==", "source_content_hash":"hash"}}),
            serde_json::json!({"verb":"trace", "trace":{"id":1,"turn":0}, "event":"plan_ready"}),
            serde_json::json!({"verb":"cluster_faces", "profile_id":"p", "model_id":"m", "images":[]}),
            serde_json::json!({"verb":"list_models", "profile_id":"p"}),
            serde_json::json!({"verb":"get_provider_config"}),
        ];
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
        let http = crate::service::provider_client().unwrap();
        for value in forbidden {
            let request = serde_json::from_value(value.clone()).unwrap();
            assert!(!chat_completion(&request), "{value}");
            // Rejection precedes any provider configuration, credential or
            // network access.
            let response =
                crate::service::handle_streaming("so.tilo.chat", request, &http, &mut |_| true)
                    .await;
            assert!(!response.ok, "{value}");
            let expected = match value["verb"].as_str().unwrap() {
                "cluster_faces" => "only Photos may request fixed local face clustering",
                "analyze_image" => "only Photos may request fixed local image analysis",
                "complete" | "complete_default" => "Provider permission denied",
                _ => "not authorized",
            };
            assert!(response.error.unwrap().contains(expected), "{value}");
        }
    }

    tilo_test_env::scenario! {
        /// Who may administer providers through the gateway.
        fn provider_management_needs_the_exact_panel_identity;

        phase provider_control_panel_identity_is_exact;
        {
            assert!(provider_admin_app("so.tilo.settings"));
            for app in [
                "settings",
                "Settings",
                "so.tilo.settings.evil",
                "so.tilo.chat",
                "so.tilo.agent",
                "*",
            ] {
                assert!(!provider_admin_app(app), "{app}");
            }
        }
    }
}
