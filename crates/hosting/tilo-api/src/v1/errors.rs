//! The one shape every refusal of the API has, and the HTTP status its code
//! goes with (`docs/developers/api.md`, Errors).

use super::answers::{ActionRow, ErrorBody};
use hyper::StatusCode;
use serde_json::Value;

/// A refusal, as the API answers it.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Box<ErrorBody>,
}

impl ApiError {
    /// `code` with `message`, at the status the code goes with.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: status_of(code),
            body: Box::new(ErrorBody {
                code: code.to_owned(),
                message: message.into(),
                ..ErrorBody::default()
            }),
        }
    }

    #[must_use]
    pub fn next(mut self, step: impl Into<String>) -> Self {
        self.body.next_steps.push(step.into());
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    /// A host answered something of another shape than the API promises.
    pub fn malformed(what: &str, why: &str) -> Self {
        Self::new(
            "unavailable",
            format!("the host answered {what} in an unexpected shape: {why}"),
        )
    }

    /// The `error` a tool's typed answer carries, with the action's row when
    /// it names one.
    pub fn of_answer(error: Option<&Value>, action: Option<&Value>) -> Self {
        let field = |name: &str| error.and_then(|error| error.get(name));
        let code = field("code").and_then(Value::as_str).unwrap_or("failed");
        let message = field("message")
            .and_then(Value::as_str)
            .unwrap_or("the call failed");
        let mut refused = Self::new(code, message);
        refused.body.next_steps = field("next_steps")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        refused.body.retry_after_ms = field("retry_after_ms").and_then(Value::as_u64);
        refused.body.action =
            action.and_then(|row| serde_json::from_value::<ActionRow>(row.clone()).ok());
        refused
    }

    /// The JSON body.
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::to_value(super::answers::Error {
            error: (*self.body).clone(),
        })
        .unwrap_or_default()
    }
}

impl From<crate::session::CallError> for ApiError {
    fn from(error: crate::session::CallError) -> Self {
        let mut refused = Self::new(error.code, error.message);
        refused.body.retry_after_ms = error.retry_after_ms;
        refused
    }
}

impl From<crate::relay::RelayError> for ApiError {
    fn from(error: crate::relay::RelayError) -> Self {
        use crate::relay::RelayError;
        match error {
            RelayError::NotFound => Self::new("not_found", "no such workspace of yours")
                .next("GET /v1/workspaces names them"),
            RelayError::Refused(reason) => Self::new("refused", reason),
            RelayError::Unavailable(reason) => Self::new("unavailable", reason),
            RelayError::RateLimited { retry_after_ms } => {
                let mut refused = Self::new("rate_limited", error.to_string());
                refused.body.retry_after_ms = Some(retry_after_ms);
                refused
            }
        }
    }
}

/// The HTTP status a refusal's code goes with.
#[must_use]
pub fn status_of(code: &str) -> StatusCode {
    match code {
        "invalid_request" | "invalid_arguments" | "api_version_unsupported" => {
            StatusCode::BAD_REQUEST
        }
        "invalid_token" | "key_revoked" | "key_expired" | "unauthorized" => {
            StatusCode::UNAUTHORIZED
        }
        "not_permitted" | "beyond_scope" => StatusCode::FORBIDDEN,
        "not_found" | "unknown_action" | "unknown_resource" | "unknown_app" => {
            StatusCode::NOT_FOUND
        }
        "method_not_allowed" => StatusCode::METHOD_NOT_ALLOWED,
        "idempotency_conflict" | "surface_changed" => StatusCode::CONFLICT,
        "too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}
