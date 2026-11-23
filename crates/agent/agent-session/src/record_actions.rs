//! `actionsV1` for apps over the remote-action path: the providers a
//! workspace's records are addressed by (`record:<app>:<record>`), the row a
//! catalogue offers an app's open in, and how a refusal of an app's API
//! reads as a receipt.

use tilo_actions::ActionFailure;
use tilo_actions::api::ApiError;
use tilo_actions::remote::{ActionCallError, CatalogueRow, ReceiptStatus, application_open_target};
use tilo_peer_state::document_frame::DocumentRoute;

pub use tilo_actions::remote::RESOURCE_PROVIDER;

#[must_use]
pub fn resource_provider(route: &DocumentRoute) -> String {
    format!("{RESOURCE_PROVIDER}:{}:{}", route.app_id, route.document)
}

pub fn resource_of(provider: &str, workspace: &str) -> Option<DocumentRoute> {
    let rest = provider
        .strip_prefix(RESOURCE_PROVIDER)?
        .strip_prefix(':')?;
    // App ids have no colon; record ids may (`uuid:…`).
    let (app_id, record) = rest.split_once(':')?;
    Some(DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: workspace.to_owned(),
        document: record.to_owned(),
    })
}

/// The row offering to open `app` on a new record in workspace `workspace`,
/// under the name people read it by (`label`).
pub(crate) fn open_row(workspace: &str, app: &str, label: &str) -> CatalogueRow {
    open_row_needing(workspace, app, label, None)
}

/// [`open_row`] of an app whose open opens nothing unless it names `needs`:
/// the row declares it, so whoever opens the app is asked for it.
pub(crate) fn open_row_needing(
    workspace: &str,
    app: &str,
    label: &str,
    needs: Option<crate::PlacedArgument>,
) -> CatalogueRow {
    let target = application_open_target(workspace, app);
    CatalogueRow::for_action(
        target.provider,
        app,
        tilo_actions::api::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: target.address,
            resource_kind: "applications.app".into(),
            resource_label: label.to_owned(),
            action_label: format!("Open {label}"),
            keywords: Vec::new(),
            icon: None,
            description: Some(needs.map_or_else(
                || format!("Open {label} on a new record in this workspace."),
                |needs| {
                    format!(
                        "Open {label} in this workspace, naming `{}`: {}.",
                        needs.name, needs.description
                    )
                },
            )),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            params: needs
                .map(|needs| {
                    tilo_actions::ActionParam::required(
                        needs.name,
                        tilo_actions::ParamType::Text,
                        needs.description,
                    )
                })
                .into_iter()
                .collect(),
            input: tilo_actions::InputSpec::None,
            view: None,
        },
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An app that opens nothing unless its open names something declares
    /// it, so whoever opens it from the listing is asked for it, rather than
    /// offered an open that can only be refused.
    #[test]
    fn an_open_that_needs_a_name_declares_it() {
        let plain = open_row("ws", "so.tilo.browser", "Browser")
            .action
            .expect("an open");
        assert!(plain.params.is_empty());
        let needing = open_row_needing(
            "ws",
            "so.tilo.linux",
            "Linux apps",
            Some(crate::PlacedArgument {
                name: "app",
                description: "the app to open",
            }),
        )
        .action
        .expect("an open");
        assert_eq!(needing.params.len(), 1);
        assert_eq!(needing.params[0].name, "app");
        assert!(needing.params[0].required);
        assert_eq!(
            needing.description.as_deref(),
            Some("Open Linux apps in this workspace, naming `app`: the app to open.")
        );
    }
}
