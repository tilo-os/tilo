//! OpenAI-compatible chat completions shared by the gateway and host-side
//! agents: which request fields a target accepts, idempotency keys, and what a
//! failed call means.
//!
//! Tilo Cloud's metered endpoint (`/api/machines/{id}/ai/v1/chat/completions`)
//! is the one target that differs from a plain provider. It bills every
//! completion against the machine's credit, so it requires a UUID
//! `Idempotency-Key` (a retry with the same key and body returns the settled
//! answer instead of charging twice), accepts a closed set of fields, and
//! answers failures as `{"error": "...", "code": "..."}`.

use std::time::Duration;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub use tilo_model_client::failure::{Failure, Kind, OUT_OF_CREDITS};

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
    /// Whether every request needs an `Idempotency-Key` header.
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
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    )
}

/// The key for turn `turn` of one planning run seeded by `run`, asking with
/// `body`.
#[must_use]
pub fn turn_key(run: &[u8], turn: u32, body: &Value) -> String {
    idempotency_key(&[run, &turn.to_le_bytes(), body.to_string().as_bytes()])
}

/// A provider error body's `type`, `param` and `code`, each only when it is
/// a short lowercase identifier: an API key or the provider's prose never
/// passes. Empty when it names none.
#[must_use]
pub fn error_fields(body: &[u8]) -> String {
    use std::fmt::Write as _;
    let parsed: Value = serde_json::from_slice(body).unwrap_or_default();
    let error = parsed
        .get("error")
        .filter(|error| error.is_object())
        .unwrap_or(&parsed);
    let mut fields = String::new();
    for name in ["type", "param", "code"] {
        let Some(value) = error.get(name).and_then(Value::as_str) else {
            continue;
        };
        if !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'.' | b'[' | b']')
            })
        {
            let _ = write!(fields, " {name}={value}");
        }
    }
    fields
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
    use serde_json::json;

    use super::*;

    tilo_test_env::scenario! {
        /// What a failed answer becomes: its log fields, its classification and the wait before asking again.
        fn provider_failures_are_classified_logged_and_waited_out_without_their_text;

        phase a_provider_refusal_is_logged_by_its_identifiers_alone;
        {
            let body = br#"{"error":{"message":"Function tools with reasoning_effort are not supported for sk-proj-SECRET","type":"invalid_request_error","param":"reasoning_effort","code":"unsupported_parameter"}}"#;
            assert_eq!(
                error_fields(body),
                " type=invalid_request_error param=reasoning_effort code=unsupported_parameter"
            );
            let hostile = br#"{"error":{"type":"sk-proj-SECRET","param":"tools[0].function.name","code":"Bearer abc"}}"#;
            assert_eq!(error_fields(hostile), " param=tools[0].function.name");
            assert_eq!(error_fields(b"<html>"), "");
            assert_eq!(
                error_fields(br#"{"type":"error","code":"rate_limit"}"#),
                " type=error code=rate_limit"
            );
        }

        phase cloud_failures_say_what_happened;
        {
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
            // An interrupted exchange may already have been billed; do not turn
            // better error classification into a newly retryable paid operation.
            assert!(!Failure::transport(false, false).retryable());
            assert!(Failure::transport(true, false).retryable());
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

        phase backoff_doubles_and_honours_a_bounded_retry_after;
        {
            assert_eq!(backoff(1, None), Duration::from_millis(500));
            assert_eq!(backoff(3, None), Duration::from_secs(2));
            assert_eq!(backoff(9, None), LONGEST_WAIT);
            assert_eq!(backoff(1, Some(Duration::from_secs(60))), LONGEST_WAIT);
        }
    }

    tilo_test_env::scenario! {
        /// The idempotency key of a turn and the body its target accepts.
        fn a_request_is_keyed_and_shaped_for_its_target;

        phase a_key_is_a_uuid_and_the_same_for_the_same_request;
        {
            let body = json!({"model": "m", "messages": []});
            let key = turn_key(b"run", 0, &body);
            assert_eq!(key.len(), 36);
            assert_eq!(key.chars().nth(14), Some('8'));
            assert_eq!(key, turn_key(b"run", 0, &body));
            assert_ne!(key, turn_key(b"run", 1, &body));
            assert_ne!(key, turn_key(b"other", 0, &body));
        }

        phase the_metered_target_drops_what_it_would_refuse;
        {
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
    }
}
