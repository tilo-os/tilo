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
