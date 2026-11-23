//! The models a workspace's work runs on, answered by this session's broker for the person at
//! this device ([`tilo_actions::remote::person_models`]).
//!
//! A workspace keeps its model providers and the model each kind of its work runs on itself,
//! wherever it is held: a host keeps them for a page, and a person's computer keeps them for
//! the workspaces it holds. A window here (Settings, the Model providers window, an assistant's
//! first minute) and a command of the person's ask for them as they would ask a host; the broker
//! admits the ask under Settings' app id and the workspace's holder answers it: this device's
//! own workspace service when it holds the workspace, else the device its enrollment names
//! ([`Engine::ask_holder_of`]). The agents of the workspace answer on what is chosen here.
//!
//! A key is never typed through here: it is kept by the vault the workspace has, and only its
//! item is named ([`person_models::SET_KEY_FROM_ITEM`]).

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::{InvokeArgument, Pending, person_models};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionOutput, ActionState,
    ExecutionLocus, InputSpec, OperationKind,
};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_desktop_model::WorkspaceId;

use crate::engine::Engine;

/// Each step, with what it is called and whether it only reads.
const STEPS: [(&str, &str, bool); 16] = [
    (
        person_models::LIST,
        "List the workspace's model providers",
        true,
    ),
    (person_models::ADD_PROVIDER, "Add a model provider", false),
    (
        person_models::ADD_PRESET,
        "Add a known model provider",
        false,
    ),
    (
        person_models::SET_KEY_FROM_ITEM,
        "Use a key the vault keeps for a provider",
        false,
    ),
    (person_models::REMOVE_KEY, "Forget a provider's key", false),
    (
        person_models::REMOVE_PROVIDER,
        "Remove a model provider",
        false,
    ),
    (
        person_models::REFRESH_MODELS,
        "Read a provider's models again",
        false,
    ),
    (person_models::ADD_MODEL, "Add a model to a provider", false),
    (
        person_models::SET_DEFAULT,
        "Choose the workspace's model",
        false,
    ),
    (
        person_models::SET_ASSIGNMENT,
        "Choose the model for a kind of work",
        false,
    ),
    (
        person_models::SET_ASSIGNMENTS,
        "Choose the models for each kind of work",
        false,
    ),
    (
        person_models::SET_REASONING,
        "Choose how long a model thinks",
        false,
    ),
    (
        person_models::LINK_THIS_COMPUTER,
        "Run the workspace on this computer's model",
        false,
    ),
    (
        person_models::BEGIN_SIGN_IN,
        "Sign in to a model provider",
        false,
    ),
    (
        person_models::WAIT_SIGN_IN,
        "Wait for a sign-in to a model provider",
        true,
    ),
    (
        person_models::CANCEL_SIGN_IN,
        "Stop signing in to a model provider",
        false,
    ),
];

/// The workspace's models, answered by its holder for the person at this device.
pub(crate) struct ModelSteps {
    engine: Option<Arc<Engine>>,
}

impl ModelSteps {
    /// The steps of a session holding `engine`, or of one that keeps no workspace and so
    /// reaches no holder.
    pub(crate) fn session(engine: Option<Arc<Engine>>) -> Arc<Self> {
        Arc::new(Self { engine })
    }
}

/// The step `action`, described for the broker's catalogue.
fn descriptor((action, label, reads): (&str, &str, bool)) -> ActionDescriptor {
    let (kind, contract) = if reads {
        (OperationKind::View, ActionContract::view())
    } else {
        (OperationKind::Effect, ActionContract::default())
    };
    ActionDescriptor {
        did: None,
        state: ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(person_models::RESOURCE, action),
        resource_kind: person_models::RESOURCE.into(),
        resource_label: "Agent models".into(),
        action_label: label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// The arguments the step carries.
fn arguments(request: &HostStepRequest<'_>) -> Result<Map<String, Value>, String> {
    match InvokeArgument::from_slot(request.argument) {
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Json(object) => Ok(object),
        InvokeArgument::Text(_) => Err("a model step takes typed arguments".to_owned()),
    }
}

/// What the holder answered, as the step's outcome.
fn outcome(asked: Result<Value, String>) -> HostStepOutcome {
    asked
        .map(|value| ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(ActionFailure {
                code: "models_refused".into(),
                message,
                details: None,
                retryable: false,
            })
        })
}

impl HostStepServices for ModelSteps {
    fn provider(&self) -> &'static str {
        person_models::PROVIDER
    }

    fn app_id(&self) -> &str {
        settings_sections::APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        STEPS.into_iter().map(descriptor).collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let (action, _, _) = STEPS
            .into_iter()
            .find(|(action, _, _)| {
                request.address.resource == person_models::RESOURCE
                    && request.address.action == *action
            })
            .ok_or("unknown model step")?;
        // The holder answers this device as its person: an agent or an app acting for them
        // chooses no model here.
        request
            .actor
            .direct_person()
            .ok_or("only you choose the models your workspace runs on")?;
        let workspace = WorkspaceId::parse(request.workspace)
            .ok_or("the models were asked in no workspace of this device")?;
        let engine = self
            .engine
            .as_ref()
            .ok_or("this session keeps no workspace, so it reaches none's models")?
            .clone();
        let arguments = arguments(request)?;
        let target = person_models::target(action);
        Ok(crate::landing::off_loop(async move {
            let asked = engine
                .ask_holder_of(workspace, target, arguments)
                .ok_or("this device reaches no holder of the workspace")?;
            asked.await
        })
        .map(outcome))
    }
}
