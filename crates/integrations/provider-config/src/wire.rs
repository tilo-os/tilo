//! One model turn on a provider's wire: the request a [`ModelRequest`]
//! becomes, and the [`ModelResponse`] read back out of the provider's
//! answer, whole or streamed.
//!
//! Every Tilo process that asks a model for a turn speaks through this one
//! codec: the per-user gateway on a Tilo computer, a host's agents, and a
//! browser page's model service. It holds no HTTP client, no key and no
//! clock, so it runs wherever those do and is tested once.
//!
//! Four wires are spoken ([`Api`]): Anthropic Messages, `OpenAI` chat
//! completions (which every OpenAI-compatible server answers), `OpenAI`
//! Responses, and the Responses variant of the `ChatGPT` backend a `ChatGPT`
//! sign-in reaches. The profile decides which ([`Api::of`]); the profile
//! also decides the request's provider-specific fields (its token-limit
//! field, its reasoning effort, the fields it refuses), so a model is
//! configured in exactly one place.
//!
//! An answer is a model's only after it validates ([`check`]): a stop
//! reason the wire names, tool calls with an id, a declared name and object
//! arguments. A streamed answer's deltas ([`Delta`]) are presentation only;
//! the response [`Stream::finish`] assembles is what may be acted on.

use serde_json::{Map, Value, json};

use crate::protocol::{
    Content, ErrorKind, ModelError, ModelRequest, ModelResponse, StopReason, ToolChoice,
};
use crate::{OpenAiApi, ProviderProfile, ReasoningParam, transform};

mod anthropic;
mod chat;
mod responses;
mod sse;

pub use sse::{MOST_BYTES, Stream, StreamError};

/// The wire a profile's turns are spoken in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    /// Anthropic's `/v1/messages`.
    Anthropic,
    /// `OpenAI`'s `/chat/completions`, which every compatible server speaks.
    Chat,
    /// `OpenAI`'s `/responses`, stateless (`store: false`).
    Responses,
    /// The `ChatGPT` backend's Responses: always streamed, never stored, and
    /// taking neither an output limit nor a temperature.
    ChatGpt,
}

impl Api {
    /// The wire `profile`'s turns take.
    #[must_use]
    pub fn of(profile: &ProviderProfile) -> Self {
        if transform::speaks_responses(profile) {
            return Self::ChatGpt;
        }
        match transform::openai_api(profile) {
            None => Self::Anthropic,
            Some(OpenAiApi::Responses) => Self::Responses,
            Some(OpenAiApi::ChatCompletions) => Self::Chat,
        }
    }

    /// Whether a turn on this wire is always answered as a stream.
    #[must_use]
    pub const fn always_streams(self) -> bool {
        matches!(self, Self::ChatGpt)
    }
}

/// Where `profile`'s turns are sent.
#[must_use]
pub fn url(profile: &ProviderProfile) -> String {
    match Api::of(profile) {
        Api::Responses | Api::ChatGpt => transform::responses_url(profile),
        Api::Anthropic | Api::Chat => transform::url(profile.protocol, &profile.endpoint),
    }
}

/// How one turn is asked besides its conversation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ask<'a> {
    /// Ask for the answer as a stream ([`Stream`]).
    pub streaming: bool,
    /// Fields a trusted caller adds to the body (a host's sampling hints).
    /// The profile's own settings and the model's output cap still win.
    pub extra: Option<&'a Map<String, Value>>,
}

/// The body asking `model` of `profile` for the next turn of `request`.
///
/// # Errors
/// [`ErrorKind::InvalidRequest`] for a conversation the wire cannot carry:
/// none at all, a tool result answering no call, a call reusing an id, an
/// image in a system message, or a reply the provider cannot continue.
pub fn body(
    profile: &ProviderProfile,
    model: &str,
    request: &ModelRequest,
    ask: Ask<'_>,
) -> Result<Value, ModelError> {
    check_request(request)?;
    let api = Api::of(profile);
    let streaming = ask.streaming || api.always_streams();
    let mut object = match api {
        Api::Anthropic => anthropic::body(request, model),
        Api::Chat => chat::body(request)?,
        Api::Responses | Api::ChatGpt => responses::body(request, api)?,
    };
    if profile.signs_in_with_google() && ends_with_model_turn(request) {
        return Err(ModelError::invalid(
            "Google Vertex AI cannot continue the model's own reply: the conversation must end \
             with a user or tool message",
        ));
    }
    if let Some(extra) = ask.extra {
        for (key, value) in extra {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    object.insert("model".into(), Value::String(model.into()));
    let cap = output_cap(profile, model, request.max_output_tokens)?;
    match api {
        Api::Anthropic => {
            object.insert("max_tokens".into(), cap.into());
            set_stream(&mut object, streaming, false);
            anthropic::cache_breakpoints(profile, &mut object);
        }
        Api::Chat => {
            object.remove("max_tokens");
            object.remove("max_completion_tokens");
            object.insert(chat_limit_field(profile).into(), cap.into());
            reasoning(profile, &mut object, false);
            set_stream(&mut object, streaming, true);
            // Google's endpoint thinks before it answers and says nothing of it unless asked;
            // where the answer streams, its thought is shown as it comes. What a caller asked
            // of Google itself stays as it asked it.
            if streaming && profile.is_googles_endpoint() {
                object.entry("extra_body").or_insert_with(
                    || json!({"google": {"thinking_config": {"include_thoughts": true}}}),
                );
            }
        }
        Api::Responses => {
            object.insert("max_output_tokens".into(), cap.into());
            reasoning(profile, &mut object, true);
            set_stream(&mut object, streaming, false);
            object.insert("store".into(), false.into());
            responses::ask_for_ciphertext(&mut object);
        }
        Api::ChatGpt => {
            for refused in ["max_output_tokens", "max_tokens", "temperature"] {
                object.remove(refused);
            }
            reasoning(profile, &mut object, true);
            set_stream(&mut object, true, false);
            object.insert("store".into(), false.into());
        }
    }
    // Last: fields this provider refuses, whoever added them, except those
    // without which there is no request.
    for param in &profile.omit_params {
        if !crate::REQUIRED_PARAMS.contains(&param.as_str()) {
            object.remove(param.as_str());
        }
    }
    Ok(Value::Object(object))
}

/// The turn out of a provider's whole (unstreamed) answer `body` to a
/// request [`body`] wrote for `profile`, checked against the tools
/// `request` declared.
///
/// # Errors
/// [`ErrorKind::InvalidResponse`] for an answer that is no model turn, or a
/// turn that fails [`check`].
pub fn response(
    profile: &ProviderProfile,
    request: &ModelRequest,
    body: &str,
) -> Result<ModelResponse, ModelError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| ModelError::invalid("the provider's answer is not JSON"))?;
    let response = decode(Api::of(profile), profile.is_googles_endpoint(), &value)?;
    check(&response, request)?;
    Ok(response)
}

/// The turn a whole answer `value` on `api` holds, unchecked; `google` when it came from
/// Google's endpoint.
pub(crate) fn decode(api: Api, google: bool, value: &Value) -> Result<ModelResponse, ModelError> {
    match api {
        Api::Anthropic => anthropic::response(value),
        Api::Chat => chat::response(value, google),
        Api::Responses | Api::ChatGpt => responses::response(value),
    }
}

/// Whether `response` is a turn that may be used as an answer to `request`:
/// it says something or calls something, and a turn that ended normally
/// calls only tools `request` declared, each once, with an id and object
/// arguments. A refused or truncated turn stays a valid answer, which no
/// caller can act on ([`ModelResponse::tool_calls`] refuses it).
///
/// # Errors
/// [`ErrorKind::InvalidResponse`], naming what was wrong.
pub fn check(response: &ModelResponse, request: &ModelRequest) -> Result<(), ModelError> {
    let said_nothing = response.message.content.iter().all(|block| match block {
        Content::Reasoning { text, .. } | Content::Text { text } => text.is_empty(),
        _ => false,
    });
    if said_nothing && response.stop_reason == StopReason::EndTurn {
        return Err(ModelError::invalid("the model answered nothing"));
    }
    if !matches!(
        response.stop_reason,
        StopReason::EndTurn | StopReason::ToolCalls
    ) {
        return Ok(());
    }
    let calls = response.tool_calls()?;
    let declared = |name: &str| {
        request.tool_choice != ToolChoice::None
            && request.tools.iter().any(|tool| tool.name == name)
    };
    if let Some(call) = calls.iter().find(|call| !declared(&call.name)) {
        return Err(ModelError::invalid(format!(
            "the model called {}, a tool it was not offered",
            crate::wire::bounded(&call.name, 64)
        )));
    }
    Ok(())
}

/// A request every wire can carry: some conversation, and every tool result
/// answering one earlier call, each call id used once.
fn check_request(request: &ModelRequest) -> Result<(), ModelError> {
    use crate::protocol::Role;
    if request.messages.is_empty() {
        return Err(ModelError::invalid("a model turn needs a conversation"));
    }
    let mut open = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for message in &request.messages {
        for content in &message.content {
            let fits = match (message.role, content) {
                (_, Content::Text { .. })
                | (Role::User, Content::Image { .. })
                | (Role::Assistant, Content::Reasoning { .. }) => true,
                (Role::Assistant, Content::ToolCall(call)) => {
                    call.validate()?;
                    seen.insert(call.id.as_str()) && open.insert(call.id.as_str())
                }
                (Role::Tool, Content::ToolResult(result)) => open.remove(result.call_id.as_str()),
                _ => false,
            };
            if !fits {
                return Err(ModelError::new(ErrorKind::InvalidRequest));
            }
        }
    }
    Ok(())
}

/// Whether the conversation ends with the model's own words (a prefill).
fn ends_with_model_turn(request: &ModelRequest) -> bool {
    request
        .messages
        .last()
        .is_some_and(|message| message.role == crate::protocol::Role::Assistant)
}

/// The most output `model` of `profile` is asked for: what the caller asked,
/// no more than the model's configured allowance.
fn output_cap(profile: &ProviderProfile, model: &str, asked: u32) -> Result<u32, ModelError> {
    let Some((descriptor, limits)) = profile
        .models
        .iter()
        .find(|descriptor| descriptor.id == model)
        .and_then(|descriptor| {
            descriptor
                .request_limits
                .as_ref()
                .map(|limits| (descriptor, limits))
        })
    else {
        return Ok(asked);
    };
    limits
        .validate(descriptor.context_window)
        .map_err(ModelError::invalid)?;
    Ok(if asked == 0 {
        limits.max_output_tokens
    } else {
        asked.min(limits.max_output_tokens)
    })
}

/// The field an OpenAI-compatible profile takes its output limit in: the one
/// it names, or `max_completion_tokens` at `OpenAI`'s own API (which refuses
/// `max_tokens` for its reasoning models) and `max_tokens` elsewhere.
fn chat_limit_field(profile: &ProviderProfile) -> &'static str {
    profile.token_limit_param.map_or_else(
        || {
            if profile.endpoint.contains("://api.openai.com") {
                "max_completion_tokens"
            } else {
                "max_tokens"
            }
        },
        crate::TokenLimitParam::key,
    )
}

/// The profile's reasoning setting, replacing any the caller sent. On a
/// Responses wire the effort is always an object.
fn reasoning(profile: &ProviderProfile, object: &mut Map<String, Value>, responses: bool) {
    if responses && let Some(effort) = object.remove("reasoning_effort") {
        object.insert("reasoning".into(), json!({ "effort": effort }));
    }
    let Some(setting) = &profile.reasoning else {
        return;
    };
    object.remove("reasoning_effort");
    object.remove("reasoning");
    let effort = Value::String(setting.effort.clone());
    match setting.param {
        ReasoningParam::Omit => {}
        ReasoningParam::ReasoningEffort if !responses => {
            object.insert("reasoning_effort".into(), effort);
        }
        ReasoningParam::ReasoningEffort | ReasoningParam::ReasoningObject => {
            object.insert("reasoning".into(), json!({ "effort": effort }));
        }
    }
}

/// `stream` as asked; with usage reported at the end of a chat stream.
fn set_stream(object: &mut Map<String, Value>, streaming: bool, usage_option: bool) {
    object.remove("stream_options");
    if streaming {
        object.insert("stream".into(), true.into());
        if usage_option {
            object.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    } else {
        object.remove("stream");
    }
}

/// One presentation update of a streamed turn. Never authority: a call's
/// arguments, even when they look complete, may still be cut or refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delta {
    /// Words of the answer.
    Text(String),
    /// Reasoning the provider chose to show.
    Reasoning(String),
    /// A tool call opened at `index` (the order calls appear in the turn).
    /// Its id and name may arrive only later, or never before it closes.
    CallOpened {
        index: usize,
        id: Option<String>,
        name: String,
    },
    /// More of the arguments of the call at `index`.
    CallArguments { index: usize, json: String },
    /// The call at `index` is complete as the provider wrote it.
    CallClosed {
        index: usize,
        id: String,
        name: String,
    },
}

/// `text` cut to `most` bytes at a character boundary, for a message that
/// quotes something a provider sent.
pub(crate) fn bounded(text: &str, most: usize) -> &str {
    let end = (0..=most.min(text.len()))
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    text.get(..end).unwrap_or_default()
}

/// `field` under `key` of `value`, when it is an object.
pub(crate) fn put(value: &mut Value, key: &str, field: Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.into(), field);
    }
}

/// A count out of a usage object.
pub(crate) fn count(usage: Option<&Value>, key: &str) -> u64 {
    usage
        .and_then(|usage| usage.pointer(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// Opaque provider state a block carries, under `key` of its extension.
pub(crate) fn extra<'a>(extra: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    extra.and_then(|extra| extra.get(key))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod thought_tests;
