//! A whole model turn in either provider dialect, for a client that talks to
//! the provider itself instead of through the per-user gateway: a browser
//! page's model service, which has no gateway to hand its windows' requests
//! to. Pure pieces like [`agent_provider_config::transform`]: the request body for a
//! [`ModelRequest`] and the [`ModelResponse`] out of the provider's answer,
//! with no HTTP client, so they are tested once wherever they run.
//!
//! The OpenAI-compatible dialect is chat completions, which every compatible
//! provider (`OpenAI` itself included) serves for text, images and function
//! tools. Turns are asked without streaming: the answer arrives whole, and
//! the caller's presentation stream gets it in one delta.

use serde_json::{Map, Value, json};

use agent_provider_config::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason,
    ToolCall, ToolChoice, Usage,
};
use agent_provider_config::{ProviderProfile, ProviderProtocol, TokenLimitParam};

/// The request body asking `model` of `profile` for the next turn of `request`.
///
/// # Errors
/// A request with no messages, or a tool call whose arguments are not an object.
pub fn body(
    profile: &ProviderProfile,
    model: &str,
    request: &ModelRequest,
) -> Result<Value, ModelError> {
    if request.messages.is_empty() {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    }
    let mut object = match profile.protocol {
        ProviderProtocol::OpenAiCompatible => openai_body(profile, request)?,
        ProviderProtocol::Anthropic => anthropic_body(request),
    };
    object.insert("model".into(), Value::String(model.into()));
    if profile.protocol == ProviderProtocol::OpenAiCompatible {
        // Last: fields this provider rejects, as the gateway drops them.
        for param in &profile.omit_params {
            if !agent_provider_config::REQUIRED_PARAMS.contains(&param.as_str()) {
                object.remove(param.as_str());
            }
        }
    }
    Ok(Value::Object(object))
}

/// The turn out of a provider's answer `body` to [`body`].
///
/// # Errors
/// An answer that is not the dialect's, or carries a malformed tool call.
pub fn response(protocol: ProviderProtocol, body: &str) -> Result<ModelResponse, ModelError> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| ModelError::new(ErrorKind::InvalidResponse))?;
    let response = match protocol {
        ProviderProtocol::OpenAiCompatible => openai_response(&value),
        ProviderProtocol::Anthropic => anthropic_response(&value),
    }
    .ok_or_else(|| ModelError::new(ErrorKind::InvalidResponse))?;
    for content in &response.message.content {
        if let Content::ToolCall(call) = content {
            call.validate()?;
        }
    }
    Ok(response)
}

fn openai_body(
    profile: &ProviderProfile,
    request: &ModelRequest,
) -> Result<Map<String, Value>, ModelError> {
    let mut messages = Vec::new();
    for message in &request.messages {
        openai_messages(message, &mut messages)?;
    }
    let mut object = Map::new();
    object.insert("messages".into(), Value::Array(messages));
    let limit = profile.token_limit_param.unwrap_or_else(|| {
        // `OpenAI`'s own API refuses `max_tokens` for its reasoning models.
        if profile.endpoint.contains("://api.openai.com") {
            TokenLimitParam::MaxCompletionTokens
        } else {
            TokenLimitParam::MaxTokens
        }
    });
    object.insert(limit.key().into(), request.max_output_tokens.into());
    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect();
        object.insert("tools".into(), Value::Array(tools));
        object.insert(
            "tool_choice".into(),
            match request.tool_choice {
                ToolChoice::Required => "required",
                _ => "auto",
            }
            .into(),
        );
    }
    Ok(object)
}

fn openai_messages(message: &Message, out: &mut Vec<Value>) -> Result<(), ModelError> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut calls = Vec::new();
    for content in &message.content {
        match content {
            Content::Text { text: piece } => {
                text.push_str(piece);
                parts.push(json!({"type": "text", "text": piece}));
            }
            Content::Image { media_type, base64 } => parts.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{media_type};base64,{base64}")},
            })),
            Content::ToolCall(call) => calls.push(json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": serde_json::to_string(&call.input)
                        .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?,
                },
            })),
            Content::ToolResult(result) => out.push(json!({
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": result.content,
            })),
            // Provider reasoning is presentation, never sent back as input.
            Content::Reasoning { .. } => {}
        }
    }
    let images = parts.len() > message.content.iter().filter(|c| is_text(c)).count();
    let content = if images {
        Value::Array(parts)
    } else if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    let role = match message.role {
        Role::System => "system",
        Role::Assistant => "assistant",
        // Results went out as their own `tool` messages above.
        Role::Tool if content.is_null() => return Ok(()),
        Role::User | Role::Tool => "user",
    };
    if role == "assistant" && !calls.is_empty() {
        out.push(json!({"role": role, "content": content, "tool_calls": calls}));
    } else if !content.is_null() {
        out.push(json!({"role": role, "content": content}));
    }
    Ok(())
}

fn is_text(content: &Content) -> bool {
    matches!(content, Content::Text { .. })
}

/// The first choice of a chat-completions answer, read as the gateway reads
/// it ([`crate::openai::read_turn`]): an unknown finish reason, or a tool
/// call without object arguments, is a malformed answer.
fn openai_response(value: &Value) -> Option<ModelResponse> {
    let turn = crate::openai::read_turn(value).ok()?;
    let mut content = Vec::new();
    if let Some(reasoning) = turn
        .message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        content.push(Content::Reasoning {
            text: reasoning.into(),
            extra_content: None,
        });
    }
    if let Some(text) = turn.text.filter(|text| !text.is_empty()) {
        content.push(Content::Text { text });
    }
    let echoed = turn.message.get("tool_calls").and_then(Value::as_array);
    for (index, call) in turn.calls.into_iter().enumerate() {
        content.push(Content::ToolCall(ToolCall {
            id: call.id,
            name: call.name,
            input: call.input,
            extra_content: echoed
                .and_then(|calls| calls.get(index))
                .and_then(|call| call.get("extra_content"))
                .cloned(),
        }));
    }
    let refused = value
        .pointer("/choices/0/message/refusal")
        .and_then(Value::as_str)
        .is_some_and(|refusal| !refusal.is_empty());
    let stop_reason = match value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        _ if refused => StopReason::Refusal,
        Some("tool_calls") => StopReason::ToolCalls,
        Some("length") => StopReason::OutputLimit,
        Some("content_filter") => StopReason::Refusal,
        _ if content.iter().any(|c| matches!(c, Content::ToolCall(_))) => StopReason::ToolCalls,
        _ => StopReason::EndTurn,
    };
    let usage = value.get("usage");
    let count = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let input_tokens = count("prompt_tokens");
    let output_tokens = count("completion_tokens");
    Some(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: Usage {
            input_tokens,
            output_tokens,
            total_tokens: count("total_tokens").max(input_tokens.saturating_add(output_tokens)),
            ..Usage::default()
        },
    })
}

fn anthropic_body(request: &ModelRequest) -> Map<String, Value> {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            system.extend(message.content.iter().filter_map(|content| match content {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            }));
            continue;
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let blocks: Vec<Value> = message
            .content
            .iter()
            .filter_map(|content| match content {
                Content::Text { text } if !text.is_empty() => {
                    Some(json!({"type": "text", "text": text}))
                }
                Content::Image { media_type, base64 } => Some(json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": base64},
                })),
                Content::ToolCall(call) => Some(json!({
                    "type": "tool_use", "id": call.id, "name": call.name, "input": call.input,
                })),
                Content::ToolResult(result) => Some(json!({
                    "type": "tool_result",
                    "tool_use_id": result.call_id,
                    "content": result.content,
                    "is_error": result.is_error,
                })),
                _ => None,
            })
            .collect();
        if blocks.is_empty() {
            continue;
        }
        // The Messages API alternates roles: a turn of the same role as the
        // one before continues it.
        match messages.last_mut() {
            Some(last) if last.get("role").and_then(Value::as_str) == Some(role) => {
                if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                    content.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let mut object = Map::new();
    object.insert("max_tokens".into(), request.max_output_tokens.into());
    if !system.is_empty() {
        object.insert("system".into(), system.join("\n\n").into());
    }
    object.insert("messages".into(), Value::Array(messages));
    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        object.insert("tools".into(), Value::Array(tools));
        object.insert(
            "tool_choice".into(),
            json!({"type": if request.tool_choice == ToolChoice::Required { "any" } else { "auto" }}),
        );
    }
    object
}

fn anthropic_response(value: &Value) -> Option<ModelResponse> {
    let mut content = Vec::new();
    for block in value.get("content")?.as_array()? {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push(Content::Text {
                text: block.get("text")?.as_str()?.into(),
            }),
            Some("thinking") => content.push(Content::Reasoning {
                text: block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                extra_content: None,
            }),
            Some("tool_use") => content.push(Content::ToolCall(ToolCall {
                id: block.get("id")?.as_str()?.into(),
                name: block.get("name")?.as_str()?.into(),
                input: block.get("input")?.clone(),
                extra_content: None,
            })),
            _ => {}
        }
    }
    let stop_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => StopReason::ToolCalls,
        Some("max_tokens") => StopReason::OutputLimit,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::EndTurn,
    };
    let usage = value.get("usage");
    let count = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let cached_input_tokens = count("cache_read_input_tokens");
    let cache_write_tokens = count("cache_creation_input_tokens");
    let input_tokens = count("input_tokens")
        .saturating_add(cached_input_tokens)
        .saturating_add(cache_write_tokens);
    let output_tokens = count("output_tokens");
    Some(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: Usage {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens.saturating_add(output_tokens),
            cached_input_tokens,
            cache_write_tokens,
            reasoning_tokens: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::protocol::{ToolDefinition, ToolResult};

    fn endpoint_profile(protocol: ProviderProtocol, endpoint: &str) -> ProviderProfile {
        ProviderProfile {
            endpoint: endpoint.into(),
            ..ProviderProfile::hosted(protocol)
        }
    }

    fn tool_turns() -> ModelRequest {
        ModelRequest {
            messages: vec![
                Message::text(Role::System, "Be brief."),
                Message::text(Role::User, "Weather?"),
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call_1".into(),
                        name: "weather".into(),
                        input: json!({"city": "Ghent"}),
                        extra_content: None,
                    })],
                },
                Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(ToolResult {
                        call_id: "call_1".into(),
                        content: "sunny".into(),
                        is_error: false,
                    })],
                },
            ],
            tools: vec![ToolDefinition {
                name: "weather".into(),
                description: "Weather in a city".into(),
                input_schema: json!({"type": "object"}),
            }],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 300,
        }
    }

    #[test]
    fn openai_body_carries_tool_turns_and_the_profiles_limit_field() {
        let profile = endpoint_profile(
            ProviderProtocol::OpenAiCompatible,
            "https://example.test/v1",
        );
        let body = body(&profile, "m", &tool_turns()).unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["max_tokens"], 300);
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "Be brief."})
        );
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Ghent\"}"
        );
        assert_eq!(
            body["messages"][3],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"})
        );
        assert_eq!(body["tools"][0]["function"]["name"], "weather");
        assert_eq!(body["tool_choice"], "auto");
        let openai = endpoint_profile(
            ProviderProtocol::OpenAiCompatible,
            "https://api.openai.com/v1",
        );
        let body = super::body(&openai, "m", &tool_turns()).unwrap();
        assert_eq!(body["max_completion_tokens"], 300);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn anthropic_body_lifts_the_system_prompt_and_pairs_tool_blocks() {
        let profile = endpoint_profile(ProviderProtocol::Anthropic, "https://api.anthropic.com");
        let body = body(&profile, "claude", &tool_turns()).unwrap();
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(body["tool_choice"], json!({"type": "auto"}));
    }

    #[test]
    fn answers_read_text_tool_calls_and_stop_reasons() {
        let openai = response(
            ProviderProtocol::OpenAiCompatible,
            r#"{"choices":[{"finish_reason":"tool_calls","message":{"content":null,
               "tool_calls":[{"id":"c","type":"function","function":{"name":"w","arguments":"{\"a\":1}"}}]}}],
               "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        )
        .unwrap();
        assert_eq!(openai.stop_reason, StopReason::ToolCalls);
        assert_eq!(openai.tool_calls().unwrap()[0].input, json!({"a": 1}));
        assert_eq!(openai.usage.total_tokens, 5);
        let anthropic = response(
            ProviderProtocol::Anthropic,
            r#"{"content":[{"type":"text","text":"Hi"}],"stop_reason":"max_tokens",
               "usage":{"input_tokens":4,"output_tokens":1}}"#,
        )
        .unwrap();
        assert_eq!(
            anthropic.message.content,
            vec![Content::Text { text: "Hi".into() }]
        );
        assert_eq!(anthropic.stop_reason, StopReason::OutputLimit);
        assert!(response(ProviderProtocol::Anthropic, "{}").is_err());
        assert!(
            response(
                ProviderProtocol::OpenAiCompatible,
                r#"{"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"w","arguments":"[1]"}}]}}]}"#
            )
            .is_err(),
            "tool arguments must be an object"
        );
        assert!(
            response(
                ProviderProtocol::OpenAiCompatible,
                r#"{"choices":[{"finish_reason":"surprise","message":{"content":"Hi"}}]}"#
            )
            .is_err(),
            "an unknown finish reason is a malformed answer"
        );
        assert!(
            response(
                ProviderProtocol::OpenAiCompatible,
                r#"{"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"w"}}]}}]}"#
            )
            .is_err(),
            "a tool call without arguments is a malformed answer"
        );
    }
}
