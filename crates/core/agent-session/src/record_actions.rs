//! `actionsV1` for apps over the remote-action path: the providers a
//! workspace's records are addressed by (`record:<app>:<record>`), the row a
//! catalogue offers an app's open in, and how a refusal of an app's API
//! reads as a receipt.

use tilo_actions::ActionFailure;
use tilo_actions::api::ApiError;
use tilo_actions::remote::{ActionCallError, ReceiptStatus};
use tilo_peer_state::document_frame::DocumentRoute;

pub use tilo_actions::remote::RESOURCE_PROVIDER;

#[must_use]
pub fn resource_provider(route: &DocumentRoute) -> String {
    format!("{RESOURCE_PROVIDER}:{}:{}", route.app_id, route.document)
}

/// How an invocation the API refused ended.
pub(crate) enum Outcome {
    /// Nothing ran.
    Rejected(ActionCallError),
    Failed(ActionFailure),
}

/// The code an API refuses an invocation the grants do not admit with.
const PERMISSION_DENIED: &str = "permission_denied";

pub(crate) fn api_outcome(error: &ApiError) -> Outcome {
    match error {
        ApiError::Unknown(_) | ApiError::UnregisteredType(_) => {
            Outcome::Rejected(ActionCallError::UnknownAction)
        }
        ApiError::ConfirmationRequired => {
            Outcome::Rejected(ActionCallError::confirmation_required())
        }
        ApiError::MissingArgument(_)
        | ApiError::UnexpectedArgument(_)
        | ApiError::InvalidArguments(_)
        | ApiError::ArgumentOutOfRange { .. } => {
            Outcome::Rejected(ActionCallError::InvalidRequest {
                reason: error.to_string(),
            })
        }
        // The grants refused it: not the caller's, whenever they ask again.
        ApiError::Unavailable { code, .. } if code == PERMISSION_DENIED => {
            Outcome::Rejected(ActionCallError::NotPermitted)
        }
        // What an action changes has not arrived from the workspace yet:
        // nothing was made, and the same call is asked again once it has.
        ApiError::Unavailable { code, .. } if code == tilo_actions::api::NOT_READY => {
            Outcome::Rejected(ActionCallError::NotReady)
        }
        ApiError::Unavailable { reason, .. } => Outcome::Rejected(ActionCallError::Unavailable {
            reason: reason.clone(),
        }),
        // The app said no, and named why: the code crosses with the words.
        ApiError::Refused { reason, .. } => Outcome::Failed(ActionFailure {
            code: error.code().to_owned(),
            message: reason.clone(),
            details: None,
            retryable: false,
        }),
        ApiError::Failed(message) => Outcome::Failed(ActionFailure {
            code: error.code().to_owned(),
            message: message.clone(),
            details: None,
            retryable: false,
        }),
    }
}

pub fn api_status(error: &ApiError) -> ReceiptStatus {
    match api_outcome(error) {
        Outcome::Rejected(error) => ReceiptStatus::Rejected { error },
        Outcome::Failed(error) => ReceiptStatus::Failed { error },
    }
}
