//! `actionsV1` for apps over the remote-action path: the agent answers
//! `tilo_actions::remote` frames other devices of the room address to it,
//! for the records it has open, through the same door its own steps use.
//!
//! - A catalogue lists one provider per open record (`record:<app>:<record>`)
//!   with every operation the record publishes, and the room's provider (`workspace:<room>`) offering
//!   `app.<app>#open` for each delegated app.
//! - An invocation runs through the app's door with the caller's arguments;
//!   an explicit `Confirmed` from the caller satisfies a confirmation step.
//!   The change is committed through the agent's session and its presence
//!   moves there, exactly as for its own steps.
//! - Invocation ids are idempotent: a retry reports the original receipt, a
//!   different request under a used id is a conflict.
//!
//! The host stamps the sending device and delivers only within the room, so
//! a caller is a device of the room's user.

use std::collections::VecDeque;

use tilo_actions::door::DoorError;
use tilo_actions::remote::{
    ActionHostError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, application_open_target, workspace_provider, workspace_provider_room,
};
use tilo_actions::{ActionFailure, ActionOutput, PROMPTED_PARAM};
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;

/// Provider token prefix of an open record: `record:<app id>:<record id>`.
pub const RECORD_PROVIDER: &str = "record";

/// Invocations remembered for retries.
const REMEMBERED: usize = 256;

/// The provider token of the record at `route`.
#[must_use]
pub fn record_provider(route: &DocumentRoute) -> String {
    format!("{RECORD_PROVIDER}:{}:{}", route.app_id, route.record)
}

pub(crate) fn record_of(provider: &str, room: &str) -> Option<DocumentRoute> {
    let rest = provider.strip_prefix(RECORD_PROVIDER)?.strip_prefix(':')?;
    // App ids have no colon; record ids may (`uuid:…`).
    let (app_id, record) = rest.split_once(':')?;
    Some(DocumentRoute {
        app_id: app_id.to_owned(),
        cluster_id: room.to_owned(),
        record: record.to_owned(),
    })
}

/// The row offering to open `app` on a new record in workspace `room`.
pub(crate) fn open_row(room: &str, app: &str) -> CatalogueRow {
    let target = application_open_target(room, app);
    CatalogueRow::for_action(
        target.provider,
        app,
        tilo_actions::door::ActionDescriptor {
            semantic_identity: None,
            address: target.address,
            resource_kind: "applications.app".into(),
            resource_label: app.to_owned(),
            action_label: format!("Open {app}"),
            description: Some(format!("Open {app} on a new record in this room.")),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            params: Vec::new(),
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
            RemoteActionMessage::RecordRequest { correlation, .. } => {
                RemoteActionMessage::RecordResponse {
                    correlation,
                    result: Err(ActionHostError::Unavailable {
                        reason: "an agent keeps no records to lend".into(),
                    }),
                }
            }
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::RecordResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. } => return,
        };
        match RemoteActionFrame::new(reply).encode() {
            Ok(bytes) => {
                if let Err(error) = self.peer.send_remote_action(self.workspace_id, from, bytes) {
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
        let room = self.room.clone();
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
                room: room.clone(),
                record_id: route.record.clone(),
                browsable: false,
            });
            for descriptor in self.survey(&route) {
                rows.push(CatalogueRow::for_action(
                    provider.clone(),
                    route.app_id.clone(),
                    descriptor,
                ));
            }
        }
        let workspace = workspace_provider(&room);
        if query
            .provider
            .as_ref()
            .is_none_or(|wanted| *wanted == workspace)
        {
            rows.extend(
                self.catalogue
                    .iter()
                    .map(|definition| definition.app_id)
                    .filter(|app| self.identity.delegation.permits_app(app))
                    .map(|app| open_row(&room, app)),
            );
            providers.push(CatalogueProvider {
                provider: workspace,
                app_id: String::new(),
                room,
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
        if workspace_provider_room(&target.provider) == Some(self.room.as_str()) {
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
        let route = record_of(&target.provider, &self.room)
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
        // their word here: the agent's door cannot tell a person's device
        // from another agent's, so an action that asks first is refused
        // and is the person's to run themselves.
        self.invoke_as(&route, &target.address, &arguments, false)
            .map(Option::unwrap_or_default)
            .map_err(|error| match error {
                Refusal::Door(error) => door_status(&error),
                Refusal::Agent(reason) => reject(ActionHostError::Refused { reason }),
            })
    }
}

/// Why an invocation did not run or failed.
pub(super) enum Refusal {
    /// The app's door refused or the action failed.
    Door(DoorError),
    /// The agent refused: paused, outside its delegation, record not open,
    /// or the change could not be committed.
    Agent(String),
}

impl Refusal {
    /// The refusal as a step's error reads.
    pub(super) fn into_text(self) -> String {
        match self {
            Self::Door(error) => format!("{}: {error}", error.code()),
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

/// How an invocation the door refused ended.
pub(crate) enum Outcome {
    /// Nothing ran.
    Rejected(ActionHostError),
    Failed(ActionFailure),
}

pub(crate) fn door_outcome(error: &DoorError) -> Outcome {
    match error {
        DoorError::Unknown(_) | DoorError::UnregisteredType(_) => {
            Outcome::Rejected(ActionHostError::UnknownAction)
        }
        DoorError::ConfirmationRequired => Outcome::Rejected(ActionHostError::ConfirmationRequired),
        DoorError::MissingArgument(_)
        | DoorError::UnexpectedArgument(_)
        | DoorError::InvalidArguments(_)
        | DoorError::ArgumentOutOfRange { .. } => {
            Outcome::Rejected(ActionHostError::InvalidRequest {
                reason: error.to_string(),
            })
        }
        DoorError::Unavailable { reason, .. } => Outcome::Rejected(ActionHostError::Unavailable {
            reason: reason.clone(),
        }),
        DoorError::Failed(message) => Outcome::Failed(ActionFailure {
            code: error.code().to_owned(),
            message: message.clone(),
            details: None,
            retryable: false,
        }),
    }
}

pub(crate) fn door_status(error: &DoorError) -> ReceiptStatus {
    match door_outcome(error) {
        Outcome::Rejected(error) => ReceiptStatus::Rejected { error },
        Outcome::Failed(error) => ReceiptStatus::Failed { error },
    }
}
