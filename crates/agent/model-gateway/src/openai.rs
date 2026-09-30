//! OpenAI-compatible chat completions shared by the gateway and host-side
//! agents: which request fields a target accepts, function-tool turns,
//! idempotency keys, and what a failed call means.
//!
//! Tilo Cloud's metered endpoint (`/api/machines/{id}/ai/v1/chat/completions`)
//! is the one target that differs from a plain provider. It bills every
//! completion against the machine's credit, so it requires a UUID
//! `Idempotency-Key` (a retry with the same key and body returns the settled
//! answer instead of charging twice), accepts a closed set of fields, and
//! answers failures as `{"error": "...", "code": "..."}`.

use std::fmt;
use std::fmt::Write as _;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// The header a metered completion is deduplicated by.
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// How every failure that ran out of credit begins, so a status line or a
/// panel can recognise it without parsing the rest.
pub const OUT_OF_CREDITS: &str = "out of AI credits";

/// The most output one metered completion may ask for.
pub const CLOUD_MAX_OUTPUT_TOKENS: u64 = 2048;

/// Top-level fields Tilo Cloud's metered endpoint accepts.
const CLOUD_FIELDS: &[&str] = &[
    "model",
    "messages",
    "max_completion_tokens",
    "max_tokens",
    "temperature",
    "top_p",
    "stop",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "response_format",
];

/// What an OpenAI-compatible endpoint accepts beyond the common core.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    /// A provider speaking the `OpenAI` API as published.
    #[default]
    Standard,
    /// Tilo Cloud's metered machine endpoint.
    CloudMetered,
}

impl Target {
    /// Whether every request needs an [`IDEMPOTENCY_HEADER`].
    #[must_use]
    pub const fn needs_idempotency_key(self) -> bool {
        matches!(self, Self::CloudMetered)
    }

    /// `body` as this target accepts it: for the metered endpoint, fields it
    /// would refuse are dropped, one output limit is kept and capped, and
    /// `response_format` is kept only when it is `text` or `json_object`.
    #[must_use]
    pub fn shape(self, mut body: Value) -> Value {
        let Self::CloudMetered = self else {
            return body;
        };
        let Some(object) = body.as_object_mut() else {
            return body;
        };
        object.retain(|name, _| CLOUD_FIELDS.contains(&name.as_str()));
        let limit = object
            .remove("max_completion_tokens")
            .or_else(|| object.remove("max_tokens"))
            .and_then(|value| value.as_u64())
            .map_or(CLOUD_MAX_OUTPUT_TOKENS, |value| {
                value.clamp(1, CLOUD_MAX_OUTPUT_TOKENS)
            });
        object.insert("max_completion_tokens".into(), limit.into());
        let plain_format = object
            .get("response_format")
            .and_then(|format| format.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "text" | "json_object"));
        if !plain_format {
            object.remove("response_format");
        }
        body
    }
}

/// A UUID-shaped key that is the same for the same `parts`: the same logical
/// request keeps its key across every retry, so a metered endpoint charges it
/// once however often the transport has to try.
#[must_use]
pub fn idempotency_key(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(digest.iter()) {
        *slot = *byte;
    }
    // RFC 9562 version 8 (custom) with the RFC variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut key = String::with_capacity(36);
    for (at, byte) in bytes.iter().enumerate() {
        if matches!(at, 4 | 6 | 8 | 10) {
            key.push('-');
        }
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// The key for turn `turn` of one planning run seeded by `run`, asking with
/// `body`.
#[must_use]
pub fn turn_key(run: &[u8], turn: u32, body: &Value) -> String {
    idempotency_key(&[run, &turn.to_le_bytes(), body.to_string().as_bytes()])
}

/// Anthropic-shaped tool definitions (`name`, `description`,
/// `input_schema`) as chat-completions function tools.
#[must_use]
pub fn functions(definitions: &Value) -> Value {
    Value::Array(
        definitions
            .as_array()
            .into_iter()
            .flatten()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").cloned().unwrap_or(Value::Null),
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                    }
                })
            })
            .collect(),
    )
}

/// One function call the model made.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The call's id, echoed on its result.
    pub id: String,
    /// The function named.
    pub name: String,
    /// Its arguments, decoded; `{}` when absent or unreadable.
    pub input: Value,
}

/// One assistant turn of a tool conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// The assistant message to send back on the next turn, holding only the
    /// fields every endpoint accepts.
    pub message: Value,
    /// Its function calls, in order.
    pub calls: Vec<ToolCall>,
    /// Its text, if it said anything.
    pub text: Option<String>,
}

/// The first choice of a chat-completions response.
///
/// # Errors
///
/// Returns why the response is not a readable assistant turn.
pub fn read_turn(response: &Value) -> Result<Turn, String> {
    let message = response
        .pointer("/choices/0/message")
        .filter(|message| message.is_object())
        .ok_or("the model's answer had no message")?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut calls = Vec::new();
    let mut echoed = Vec::new();
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a tool call had no id")?;
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .ok_or("a tool call had no function name")?;
        let arguments = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        calls.push(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            input: serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
        });
        echoed.push(json!({
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": arguments },
        }));
    }
    let message = if echoed.is_empty() {
        json!({ "role": "assistant", "content": text.clone().unwrap_or_default() })
    } else {
        json!({ "role": "assistant", "content": text, "tool_calls": echoed })
    };
    Ok(Turn {
        message,
        calls,
        text,
    })
}

/// A tool result message answering `call`.
#[must_use]
pub fn tool_result(call: &ToolCall, content: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": call.id, "content": content })
}

/// What went wrong with a completion, as far as retrying and telling a
/// person are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The machine's included credit cannot cover the request (HTTP 402).
    OutOfCredits,
    /// Metered AI needs an active subscription (HTTP 402).
    SubscriptionRequired,
    /// Too many requests; worth asking again after a pause (HTTP 429).
    RateLimited,
    /// The endpoint or its provider is briefly unavailable (408, 5xx, a
    /// timeout or a refused connection); worth asking again.
    Busy,
    /// The endpoint cannot tell yet whether this request was answered and
    /// billed (HTTP 409 `request_pending`). Asking again cannot help.
    Pending,
    /// The credential was refused (HTTP 401/403).
    Unauthorized,
    /// The endpoint is switched off or unconfigured.
    Unavailable,
    /// The request itself was refused (other 4xx).
    Rejected,
}

/// A failed completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What it means.
    pub kind: Kind,
    /// The endpoint's own machine-readable code, if it gave one.
    pub code: Option<String>,
    /// A short explanation safe to show a person.
    pub detail: String,
    /// How long the endpoint asked to be left alone.
    pub retry_after: Option<Duration>,
}

impl Failure {
    /// Whether asking again with the same idempotency key may succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self.kind, Kind::RateLimited | Kind::Busy)
    }

    /// A transport failure before any answer arrived.
    #[must_use]
    pub fn transport(timed_out: bool, connect: bool) -> Self {
        Self {
            kind: if timed_out || connect {
                Kind::Busy
            } else {
                Kind::Rejected
            },
            code: None,
            detail: if timed_out {
                "the model endpoint timed out".into()
            } else {
                "the model endpoint could not be reached".into()
            },
            retry_after: None,
        }
    }

    /// Classify an unsuccessful HTTP answer. Only Tilo Cloud's own fixed
    /// messages are repeated; a provider's free-text message can quote a
    /// credential, so only its code is kept.
    #[must_use]
    pub fn from_http(status: u16, body: &str, retry_after: Option<&str>) -> Self {
        let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let (code, message) = match parsed.get("error") {
            Some(Value::String(message)) => (
                parsed
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                Some(message.chars().take(200).collect::<String>()),
            ),
            Some(Value::Object(error)) => (
                error
                    .get("code")
                    .or_else(|| error.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                None,
            ),
            _ => (None, None),
        };
        let kind = match (status, code.as_deref()) {
            (402, Some("subscription_required")) => Kind::SubscriptionRequired,
            (402, _)
            | (_, Some("insufficient_credit" | "credit_exhausted" | "insufficient_quota")) => {
                Kind::OutOfCredits
            }
            (409, Some("request_pending")) => Kind::Pending,
            (429, _) => Kind::RateLimited,
            (401 | 403, _) => Kind::Unauthorized,
            (
                503,
                Some(
                    "service_disabled"
                    | "pricing_unavailable"
                    | "pricing_disabled"
                    | "provider_unavailable",
                ),
            ) => Kind::Unavailable,
            (408 | 500 | 502 | 503 | 504 | 529, _) => Kind::Busy,
            _ => Kind::Rejected,
        };
        let detail = match kind {
            Kind::OutOfCredits => format!(
                "{OUT_OF_CREDITS}: this machine's included credit cannot cover the request. \
                 Add credit or connect your own API key in AI & credits"
            ),
            Kind::SubscriptionRequired => {
                "metered AI needs an active subscription for this machine".into()
            }
            Kind::Pending => "the model call's outcome is pending reconciliation; \
                 it was not retried so it cannot be charged twice"
                .into(),
            _ => message.unwrap_or_else(|| format!("the model endpoint answered HTTP {status}")),
        };
        Self {
            kind,
            code,
            detail,
            retry_after: retry_after
                .and_then(|value| value.trim().parse::<f64>().ok())
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok()),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) if !self.detail.contains(code.as_str()) => {
                write!(formatter, "{} ({code})", self.detail)
            }
            _ => formatter.write_str(&self.detail),
        }
    }
}

/// How many times one turn is asked before giving up.
pub const ATTEMPTS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_millis(500);
const LONGEST_WAIT: Duration = Duration::from_secs(8);

/// The pause before retry `retry` (1-based): what the endpoint asked for, up
/// to a bound, else a doubling backoff.
#[must_use]
pub fn backoff(retry: u32, asked_for: Option<Duration>) -> Duration {
    if let Some(wait) = asked_for {
        return wait.min(LONGEST_WAIT);
    }
    FIRST_WAIT
        .checked_mul(
            1_u32
                .checked_shl(retry.saturating_sub(1))
                .unwrap_or(u32::MAX),
        )
        .unwrap_or(LONGEST_WAIT)
        .min(LONGEST_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_a_uuid_and_the_same_for_the_same_request() {
        let body = json!({"model": "m", "messages": []});
        let key = turn_key(b"run", 0, &body);
        assert_eq!(key.len(), 36);
        assert_eq!(key.chars().nth(14), Some('8'));
        assert_eq!(key, turn_key(b"run", 0, &body));
        assert_ne!(key, turn_key(b"run", 1, &body));
        assert_ne!(key, turn_key(b"other", 0, &body));
    }

    #[test]
    fn the_metered_target_drops_what_it_would_refuse() {
        let body = json!({
            "model": "m", "messages": [], "tools": [], "tool_choice": "required",
            "max_tokens": 4096, "stream_options": {}, "logprobs": true,
            "response_format": {"type": "json_schema"}, "user": "x",
        });
        let shaped = Target::CloudMetered.shape(body.clone());
        assert_eq!(
            shaped,
            json!({"model": "m", "messages": [], "tools": [], "tool_choice": "required",
                   "max_completion_tokens": 2048})
        );
        assert_eq!(Target::Standard.shape(body.clone()), body);
        let json_object = Target::CloudMetered
            .shape(json!({"model": "m", "response_format": {"type": "json_object"}}));
        assert_eq!(json_object["response_format"]["type"], "json_object");
    }

    #[test]
    fn cloud_failures_say_what_happened() {
        let broke = Failure::from_http(
            402,
            r#"{"error":"Included credit is insufficient for this request","code":"insufficient_credit"}"#,
            None,
        );
        assert_eq!(broke.kind, Kind::OutOfCredits);
        assert!(!broke.retryable());
        assert!(broke.to_string().starts_with(OUT_OF_CREDITS));
        assert!(broke.to_string().contains("insufficient_credit"));
        let pending = Failure::from_http(409, r#"{"error":"x","code":"request_pending"}"#, None);
        assert_eq!(pending.kind, Kind::Pending);
        assert!(!pending.retryable());
        let limited = Failure::from_http(429, "", Some("2"));
        assert!(limited.retryable());
        assert_eq!(limited.retry_after, Some(Duration::from_secs(2)));
        let busy = Failure::from_http(
            503,
            r#"{"error":"AI service unavailable; retain this request ID","code":"service_unavailable"}"#,
            None,
        );
        assert!(busy.retryable());
        let off = Failure::from_http(503, r#"{"error":"x","code":"service_disabled"}"#, None);
        assert!(!off.retryable());
        let quoted = Failure::from_http(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-abc","code":"invalid_api_key"}}"#,
            None,
        );
        assert!(!quoted.to_string().contains("sk-abc"));
        assert_eq!(quoted.kind, Kind::Unauthorized);
    }

    #[test]
    fn a_tool_turn_is_read_and_echoed_in_the_accepted_shape() {
        let turn = read_turn(&json!({"choices": [{"message": {
            "role": "assistant", "content": null, "refusal": null,
            "tool_calls": [{"id": "c1", "type": "function", "index": 0,
                "function": {"name": "plan", "arguments": "{\"why\":\"x\"}"}}]
        }, "finish_reason": "tool_calls"}]}))
        .unwrap();
        assert_eq!(turn.calls[0].name, "plan");
        assert_eq!(turn.calls[0].input["why"], "x");
        assert_eq!(
            turn.message,
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "plan", "arguments": "{\"why\":\"x\"}"}}]})
        );
    }

    #[test]
    fn backoff_doubles_and_honours_a_bounded_retry_after() {
        assert_eq!(backoff(1, None), Duration::from_millis(500));
        assert_eq!(backoff(3, None), Duration::from_secs(2));
        assert_eq!(backoff(9, None), LONGEST_WAIT);
        assert_eq!(backoff(1, Some(Duration::from_secs(60))), LONGEST_WAIT);
    }
}
