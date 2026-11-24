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

    /// Say how long to wait before the same call works.
    #[must_use]
    pub fn retry_after_ms(mut self, wait: u64) -> Self {
        self.body.retry_after_ms = Some(wait);
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

    /// The JSON body, its message and steps naming this surface's routes
    /// rather than an MCP client's tools.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut body = (*self.body).clone();
        body.message = super::names::said(&body.message, super::names::Surface::Rest);
        body.next_steps = body
            .next_steps
            .iter()
            .map(|step| super::names::said(step, super::names::Surface::Rest))
            .collect();
        serde_json::to_value(super::answers::Error { error: body }).unwrap_or_default()
    }
}

impl From<crate::server::ServerError> for ApiError {
    fn from(error: crate::server::ServerError) -> Self {
        use crate::server::ServerError;
        match error {
            ServerError::NotFound => Self::new("not_found", "no such workspace of yours")
                .next("account/workspaces#list names them"),
            // The host's own wording stays in the log; the caller reads
            // what it can do about the refusal.
            ServerError::Refused(_) | ServerError::RefusedWorkspace(_) => {
                Self::new("refused", error.to_string())
            }
            ServerError::Unavailable(_) => {
                Self::new("unavailable", error.to_string()).next("try again in a minute")
            }
            ServerError::NoAccountHere => Self::new("not_permitted", error.to_string()),
            ServerError::KeyRevoked => {
                Self::new("key_revoked", error.to_string()).next(crate::APPROVE_AGAIN)
            }
            // The message names the person's key and what makes them known to the server.
            ServerError::NotOnThisServer(_) => {
                Self::new(tilo_actions::remote::NOT_ON_THIS_SERVER, error.to_string()).next(
                    "ask the server's operator to declare the person's key an owner, or have \
                     someone there send the person an invitation link",
                )
            }
            // Never "unreached": the person is asked, and it is done once they allow it.
            ServerError::AwaitingApproval(_) => {
                Self::new(crate::v1::AWAITING_APPROVAL, error.to_string())
            }
            // Nothing that can read the workspace answered: what to do is in
            // the message itself.
            ServerError::NoHolder => Self::new("unavailable", error.to_string())
                .next("open Tilo on a device that holds the workspace, then try again"),
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
        "not_permitted" | "not_offered" | "beyond_scope" | "not_on_this_server" => {
            StatusCode::FORBIDDEN
        }
        "not_found" | "unknown_action" | "unknown_resource" | "unknown_app" => {
            StatusCode::NOT_FOUND
        }
        "method_not_allowed" => StatusCode::METHOD_NOT_ALLOWED,
        "idempotency_conflict" | "surface_changed" | "offset_mismatch" | "upload_complete" => {
            StatusCode::CONFLICT
        }
        "too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        // A workspace that was not serving its session yet, and the same call unanswered: the
        // call waited for it and it did not come, so the caller asks again rather than fixing
        // anything. Never 422, which says the request itself was wrong.
        "not_ready" | "unanswered" | "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}
