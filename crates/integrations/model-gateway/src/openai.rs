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
