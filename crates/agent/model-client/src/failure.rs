use serde_json::Value;
use std::{fmt, time::Duration};

/// How every failure that ran out of credit begins, so a status line or a
/// panel can recognise it without parsing the rest.
pub const OUT_OF_CREDITS: &str = "out of AI credits";

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
    PermissionDenied,
    ContextLimit,
    TimedOut,
    /// Connection failed before dispatch; unlike interrupted transport, safe to retry.
    Connection,
    Transport,
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
    /// Convert only typed classifications, never provider-controlled prose.
    pub fn model_error(&self) -> agent_provider_config::protocol::ModelError {
        use agent_provider_config::protocol::{ErrorKind, ModelError};
        ModelError::new(match self.kind {
            Kind::OutOfCredits | Kind::SubscriptionRequired => ErrorKind::CreditsExhausted,
            Kind::Unauthorized => ErrorKind::Authentication,
            Kind::PermissionDenied => ErrorKind::PermissionDenied,
            Kind::ContextLimit => ErrorKind::ContextLimit,
            Kind::TimedOut => ErrorKind::TimedOut,
            Kind::Connection | Kind::Transport => ErrorKind::Transport,
            Kind::RateLimited => ErrorKind::RateLimited,
            Kind::Unavailable => ErrorKind::Unavailable,
            Kind::Rejected => ErrorKind::InvalidRequest,
            Kind::Busy | Kind::Pending => ErrorKind::Provider,
        })
    }

    /// Whether asking again with the same idempotency key may succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self.kind,
            Kind::RateLimited | Kind::Busy | Kind::TimedOut | Kind::Connection
        )
    }

    /// Whether HTTP refusal proves no inference was admitted. Billing failures
    /// never authorize replay, regardless of their HTTP status.
    pub fn refused_unprocessed(&self, status: u16) -> bool {
        matches!(status, 429 | 529) && matches!(self.kind, Kind::RateLimited | Kind::Busy)
    }

    /// A transport failure before any answer arrived.
    #[must_use]
    pub fn transport(timed_out: bool, connect: bool) -> Self {
        Self {
            kind: if timed_out {
                Kind::TimedOut
            } else if connect {
                Kind::Connection
            } else {
                Kind::Transport
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
        let error = parsed.get("error").unwrap_or(&parsed);
        // Provider-controlled strings are never diagnostics, including "code".
        // Keep only recognized, fixed machine codes.
        let codes = [error.get("code"), error.get("type"), parsed.get("code")];
        // Billing wins even if a provider also labels the envelope rate-limited.
        let code = codes
            .iter()
            .filter_map(|value| value.and_then(Value::as_str))
            .find(|code| billing_code(code))
            .or_else(|| {
                codes
                    .iter()
                    .filter_map(|value| value.and_then(Value::as_str))
                    .find(|code| recognized_code(code))
            })
            .map(str::to_owned);
        // Some compatible endpoints omit machine codes. Recognize only fixed
        // billing/context phrases here, never repeat the provider's prose.
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect::<String>()
            .to_ascii_lowercase();
        let kind = match (status, code.as_deref()) {
            _ if message.contains("credit balance is too low")
                || message.contains("insufficient credits") =>
            {
                Kind::OutOfCredits
            }
            (_, Some("subscription_required")) => Kind::SubscriptionRequired,
            (402, _) => Kind::OutOfCredits,
            (_, Some(code)) if billing_code(code) => Kind::OutOfCredits,
            (
                _,
                Some("context_length_exceeded" | "context_window_exceeded" | "prompt_too_long"),
            )
            | (413, _) => Kind::ContextLimit,
            (_, Some("authentication_error" | "invalid_api_key")) => Kind::Unauthorized,
            (_, Some("permission_error")) => Kind::PermissionDenied,
            (_, Some("overloaded_error")) => Kind::Busy,
            (409, Some("request_pending")) => Kind::Pending,
            (_, Some("rate_limit_error" | "rate_limit_exceeded")) | (429, _) => Kind::RateLimited,
            (401, _) => Kind::Unauthorized,
            (403, _) => Kind::PermissionDenied,
            (
                503,
                Some(
                    "service_disabled"
                    | "pricing_unavailable"
                    | "pricing_disabled"
                    | "provider_unavailable",
                ),
            ) => Kind::Unavailable,
            _ if message.contains("prompt is too long")
                || message.contains("maximum context length") =>
            {
                Kind::ContextLimit
            }
            (408 | 504, _) => Kind::TimedOut,
            (500 | 502 | 503 | 529, _) => Kind::Busy,
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
            _ => format!("the model endpoint answered HTTP {status}"),
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

fn recognized_code(code: &str) -> bool {
    matches!(
        code,
        "request_pending"
            | "service_disabled"
            | "pricing_unavailable"
            | "pricing_disabled"
            | "provider_unavailable"
            | "context_length_exceeded"
            | "context_window_exceeded"
            | "invalid_api_key"
            | "prompt_too_long"
            | "authentication_error"
            | "permission_error"
            | "rate_limit_error"
            | "rate_limit_exceeded"
            | "overloaded_error"
    )
}

fn billing_code(code: &str) -> bool {
    matches!(
        code,
        "subscription_required"
            | "insufficient_credit"
            | "insufficient_credits"
            | "credit_exhausted"
            | "credits_exhausted"
            | "insufficient_quota"
            | "credit_balance_too_low"
            | "billing_hard_limit_reached"
    )
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
