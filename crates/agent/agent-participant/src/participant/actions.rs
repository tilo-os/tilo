//! `actionsV1` for apps over the remote-action path: the agent answers
//! `tilo_actions::remote` frames other devices of the workspace address to it,
//! for the records it has open, through the same API its own steps use.
//!
//! - A catalogue lists one provider per open record (`record:<app>:<record>`)
//!   with every operation the record publishes, and the workspace's provider (`workspace:<workspace>`) offering
//!   `app.<app>#open` for each delegated app.
//! - An invocation runs through the app's API with the caller's arguments;
//!   an explicit `Confirmed` from the caller satisfies a confirmation step.
//!   The change is committed through the agent's session and its presence
//!   moves there, exactly as for its own steps.
//! - Invocation ids are idempotent: a retry reports the original receipt, a
//!   different request under a used id is a conflict.
//!
//! The host stamps the sending device and delivers only within the workspace, so
//! a caller is a device of the workspace's user.

use std::collections::VecDeque;
use tilo_session_client::{Cmd, Outbox};

use tilo_actions::api::ApiError;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, application_open_target, provider_workspace, workspace_provider,
};
use tilo_actions::{ActionFailure, ActionOutput, PROMPTED_PARAM};
use tilo_peer_state::document_frame::DocumentRoute;

use super::AgentParticipant;

pub use tilo_actions::remote::RECORD_PROVIDER;

/// Invocations remembered for retries.
const REMEMBERED: usize = 256;

#[must_use]
pub fn record_provider(route: &DocumentRoute) -> String {
    format!("{RECORD_PROVIDER}:{}:{}", route.app_id, route.record)
}

pub(crate) fn record_of(provider: &str, workspace: &str) -> Option<DocumentRoute> {
    let rest = provider.strip_prefix(RECORD_PROVIDER)?.strip_prefix(':')?;
    // App ids have no colon; record ids may (`uuid:…`).
    let (app_id, record) = rest.split_once(':')?;
    Some(DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: workspace.to_owned(),
        record: record.to_owned(),
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

/// Answered invocations, oldest first.
#[derive(Default)]
pub(super) struct Answered(VecDeque<(InvokeRequest, InvocationReceipt)>);

impl AgentParticipant {
    /// Answer one remote-action frame `from` sent, if it is a request.
    #[expect(
        clippy::too_many_lines,
        reason = "one answer for each message of the wire's vocabulary, in its order"
    )]
    pub(super) fn answer_remote_action(&mut self, from: [u8; 32], frame: &[u8]) {
        let Ok(frame) = RemoteActionFrame::decode(frame) else {
            return;
        };
        let reply = match frame.message {
            RemoteActionMessage::FeedbackRequest { correlation, .. } => {
                RemoteActionMessage::FeedbackResponse {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: "agents do not own canonical feedback delivery".into(),
                    }),
                }
            }
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result: Ok(self.catalogue(&query)),
                }
            }
            RemoteActionMessage::Invoke(request) => {
                RemoteActionMessage::Receipt(self.invoke_remote(request))
            }
            RemoteActionMessage::Observe { invocation, .. } => RemoteActionMessage::Update {
                invocation,
                state: None,
            },
            RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
                invocation,
                result: Err(ActionHostError::NotCancellable {
                    reason: "an app action runs to completion when invoked".into(),
                }),
            },
            RemoteActionMessage::RecordRead { correlation, .. } => {
                RemoteActionMessage::RecordReading {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: "an agent keeps no records to lend".into(),
                    }),
                }
            }
            // A question travels between a person's own devices; an agent
            // neither asks it nor answers it.
            RemoteActionMessage::ConfirmationRequest { correlation, .. } => {
                RemoteActionMessage::ConfirmationResponse {
                    correlation,
                    result: Err(ActionHostError::confirmation_required()),
                }
            }
            // Nor does an agent confirm an action by its own word.
            RemoteActionMessage::Confirm { correlation, .. } => {
                RemoteActionMessage::ConfirmResponse {
                    correlation,
                    result: Err(ActionHostError::confirmation_required()),
                }
            }
            // An agent holds no workspace of its own to search or follow.
            message @ (RemoteActionMessage::SearchRequest { .. }
            | RemoteActionMessage::ChangesRequest { .. }) => {
                let Some(refused) = tilo_actions::remote::refuse(
                    &message,
                    ActionHostError::Unavailable {
                        reason: "an agent keeps no records of a workspace".into(),
                    },
                ) else {
                    return;
                };
                refused
            }
            // An agent keeps no content for others to put into.
            RemoteActionMessage::ContentPutRequest { correlation, .. } => {
                RemoteActionMessage::ContentPutResponse {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: tilo_actions::remote::CONTENT_PUTS_UNAVAILABLE.into(),
                    }),
                }
            }
            // Nor any for another device to read out of.
            RemoteActionMessage::ContentGetRequest { correlation, .. } => {
                RemoteActionMessage::ContentGetResponse {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: tilo_actions::remote::CONTENT_GETS_UNAVAILABLE.into(),
                    }),
                }
            }
            RemoteActionMessage::ContentLinkRequest { correlation, .. } => {
                RemoteActionMessage::ContentLinkResponse {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: tilo_actions::remote::CONTENT_LINKS_UNAVAILABLE.into(),
                    }),
                }
            }
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::ContentLinkResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::RecordReading { .. }
            | RemoteActionMessage::RetiredRecordRequest
            | RemoteActionMessage::RetiredRecordResponse
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
            | RemoteActionMessage::RetiredResourceWatch { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::SearchResponse { .. }
            | RemoteActionMessage::ChangesResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return,
        };
        match RemoteActionFrame::new(reply).encode() {
            Ok(bytes) => {
                if let Err(error) =
                    self.peer
                        .send(Cmd::RemoteAction(self.workspace_id, from, bytes))
                {
                    tracing::warn!(%error, "agent: a remote-action answer was not sent");
                }
            }
            Err(error) => tracing::warn!(%error, "agent: a remote-action answer did not encode"),
        }
    }

    fn revision(&self) -> String {
        self.generation.to_string()
    }

    fn catalogue(&self, query: &CatalogueQuery) -> CataloguePage {
        let workspace = self.workspace.clone();
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        let routes: Vec<DocumentRoute> = self.apps.keys().cloned().collect();
        for route in routes {
            let provider = record_provider(&route);
            if query
                .provider
                .as_ref()
                .is_some_and(|wanted| *wanted != provider)
            {
                continue;
            }
            providers.push(CatalogueProvider {
                provider: provider.clone(),
                app_id: route.app_id.clone(),
                workspace: workspace.clone(),
                record_id: route.record.clone(),
                browsable: false,
            });
            for mut row in self.survey_rows(&route) {
                // The rows keep the resource each stands under: a query
                // naming one resource of the record lists its children.
                row.provider.clone_from(&provider);
                row.app_id.clone_from(&route.app_id);
                rows.push(row);
            }
        }
        let provider = workspace_provider(&workspace);
        if query
            .provider
            .as_ref()
            .is_none_or(|wanted| *wanted == provider)
        {
            rows.extend(
                self.catalogue
                    .iter()
                    .filter(|definition| self.identity.delegation.permits_app(definition.app_id()))
                    .map(|definition| open_row(&provider, definition.app_id(), definition.title())),
            );
            providers.push(CatalogueProvider {
                provider,
                app_id: String::new(),
                workspace,
                record_id: String::new(),
                browsable: false,
            });
        }
        let mut page = CataloguePage::from_rows(self.revision(), rows, query);
        if query.provider.is_none() {
            page.providers = providers;
        }
        page
    }

    fn invoke_remote(&mut self, request: InvokeRequest) -> InvocationReceipt {
        if let Some((earlier, receipt)) = self
            .answered
            .0
            .iter()
            .find(|(earlier, _)| earlier.invocation == request.invocation)
        {
            return if earlier.same_invocation(&request) {
                receipt.clone()
            } else {
                rejected(&request, ActionHostError::InvocationConflict)
            };
        }
        let status = match self.run_remote(&request) {
            Ok(output) => ReceiptStatus::Completed { output },
            Err(status) => status,
        };
        let receipt = InvocationReceipt {
            invocation: request.invocation,
            status,
        };
        if self.answered.0.len() >= REMEMBERED {
            self.answered.0.pop_front();
        }
        self.answered.0.push_back((request, receipt.clone()));
        receipt
    }

    #[allow(
        clippy::result_large_err,
        reason = "the error is the receipt status the host records as it is"
    )]
    fn run_remote(&mut self, request: &InvokeRequest) -> Result<ActionOutput, ReceiptStatus> {
        let reject = |error| ReceiptStatus::Rejected { error };
        if !self.control.may_act() {
            return Err(reject(ActionHostError::Unavailable {
                reason: "the agent is paused or stopped".into(),
            }));
        }
        if request
            .revision
            .as_ref()
            .is_some_and(|revision| *revision != self.revision())
        {
            return Err(reject(ActionHostError::StaleRevision));
        }
        let target = &request.target;
        if provider_workspace(&target.provider) == Some(self.workspace.as_str()) {
            let app = target
                .address
                .resource
                .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                .filter(|_| target.address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION)
                .ok_or_else(|| reject(ActionHostError::UnknownAction))?
                .to_owned();
            if !self.identity.delegation.permits_app(&app) {
                return Err(reject(ActionHostError::UnknownAction));
            }
            let route = self
                .open(&app, None)
                .map_err(|reason| reject(ActionHostError::Refused { reason }))?;
            return Ok(ActionOutput {
                value: Some(
                    serde_json::json!({ "provider": record_provider(&route), "record": route.record }),
                ),
                resources: Vec::new(),
            });
        }
        let route = record_of(&target.provider, &self.workspace)
            .filter(|route| self.apps.contains_key(route))
            .ok_or_else(|| reject(ActionHostError::UnknownAction))?;
        let arguments = match &request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Text(text) => {
                let mut arguments = serde_json::Map::new();
                arguments.insert(PROMPTED_PARAM.into(), text.clone().into());
                arguments
            }
            InvokeArgument::Json(arguments) => arguments.clone(),
        };
        // Whoever drives the agent's records from afar confirms nothing by
        // their word here: the agent's API cannot tell a person's device
        // from another agent's, so an action that asks first is refused
        // and is the person's to run themselves.
        self.invoke_as(&route, &target.address, &arguments, false)
            .map(Option::unwrap_or_default)
            .map_err(|error| match error {
                Refusal::Api(error) => api_status(&error),
                Refusal::Agent(reason) => reject(ActionHostError::Refused { reason }),
            })
    }
}

/// Why an invocation did not run or failed.
pub(super) enum Refusal {
    /// The app's API refused or the action failed.
    Api(ApiError),
    /// The agent refused: paused, outside its delegation, record not open,
    /// or the change could not be committed.
    Agent(String),
}

impl Refusal {
    /// The refusal as a step's error reads.
    pub(super) fn into_text(self) -> String {
        match self {
            Self::Api(error) => format!("{}: {error}", error.code()),
            Self::Agent(reason) => reason,
        }
    }
}

fn rejected(request: &InvokeRequest, error: ActionHostError) -> InvocationReceipt {
    InvocationReceipt {
        invocation: request.invocation,
        status: ReceiptStatus::Rejected { error },
    }
}

/// How an invocation the API refused ended.
pub(crate) enum Outcome {
    /// Nothing ran.
    Rejected(ActionHostError),
    Failed(ActionFailure),
}

/// The code an API refuses an invocation the grants do not admit with.
const PERMISSION_DENIED: &str = "permission_denied";

pub(crate) fn api_outcome(error: &ApiError) -> Outcome {
    match error {
        ApiError::Unknown(_) | ApiError::UnregisteredType(_) => {
            Outcome::Rejected(ActionHostError::UnknownAction)
        }
        ApiError::ConfirmationRequired => {
            Outcome::Rejected(ActionHostError::confirmation_required())
        }
        ApiError::MissingArgument(_)
        | ApiError::UnexpectedArgument(_)
        | ApiError::InvalidArguments(_)
        | ApiError::ArgumentOutOfRange { .. } => {
            Outcome::Rejected(ActionHostError::InvalidRequest {
                reason: error.to_string(),
            })
        }
        // The grants refused it: not the caller's, whenever they ask again.
        ApiError::Unavailable { code, .. } if code == PERMISSION_DENIED => {
            Outcome::Rejected(ActionHostError::NotPermitted)
        }
        ApiError::Unavailable { reason, .. } => Outcome::Rejected(ActionHostError::Unavailable {
            reason: reason.clone(),
        }),
        ApiError::Failed(message) => Outcome::Failed(ActionFailure {
            code: error.code().to_owned(),
            message: message.clone(),
            details: None,
            retryable: false,
        }),
    }
}

pub(crate) fn api_status(error: &ApiError) -> ReceiptStatus {
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
