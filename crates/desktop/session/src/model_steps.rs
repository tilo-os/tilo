//! The models a workspace's work runs on, answered by this session's broker for the person at
//! this device ([`tilo_workspace_records::models`]).
//!
//! A workspace keeps its model providers and the model each kind of its work runs on itself,
//! wherever it is held: a host keeps them for a page, and a person's computer keeps them for
//! the workspaces it holds. A window here (Settings, the Model providers window, an assistant's
//! first minute) and a command of the person's ask for them as they would ask a host; the broker
//! admits the ask under Settings' app id and the device's daemon answers it in that workspace
//! ([`WorkspaceView::ask_in`]): where it keeps the workspace, or of the device keeping it. The
//! agents of the workspace answer on what is chosen here.
//!
//! A key typed in the person's own window is carried here to the workspace's host, which seals
//! it in the workspace's vault: a reading never says what it is, and no other app of the
//! workspace is shown it. A key the vault already keeps is named by its item
//! ([`model_providers::SET_KEY_FROM_ITEM`]) instead.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionOutput, ActionState,
    ExecutionLocus, InputSpec, OperationKind,
};
use tilo_broker_core::BrokerActor;
use tilo_desktop_model::WorkspaceId;
use tilo_resource::Effect as _;
use tilo_workspace_records::models::{self as model_providers, ModelsAction};

use crate::workspace_view::WorkspaceView;

/// The steps the holder answers for this device: each action the providers declare but a turn
/// of the model, which a window asks of the workspace's host itself.
fn steps() -> impl Iterator<Item = tilo_resource::EffectDecl> {
    ModelsAction::declarations()
        .into_iter()
        .filter(|decl| decl.id != model_providers::ASK)
}

/// The workspace's models, answered by its holder for the person at this device.
pub(crate) struct ModelSteps {
    view: Option<Arc<WorkspaceView>>,
}

impl ModelSteps {
    /// The steps of a session reaching the device's daemon through `view`, or of one that reaches
    /// none and so no workspace.
    pub(crate) fn session(view: Option<Arc<WorkspaceView>>) -> Arc<Self> {
        Arc::new(Self { view })
    }
}

/// The step `decl` declares, described for the broker's catalogue.
fn descriptor(decl: &tilo_resource::EffectDecl) -> ActionDescriptor {
    let (kind, contract) = if decl.action_kind == tilo_actions::ActionKind::Read {
        (OperationKind::View, ActionContract::view())
    } else {
        (OperationKind::Effect, ActionContract::default())
    };
    ActionDescriptor {
        did: None,
        state: ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(model_providers::RESOURCE, decl.id),
        resource_kind: model_providers::RESOURCE.into(),
        resource_label: "Agent models".into(),
        action_label: decl.label.read(),
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
fn arguments(argument: &InvokeArgument) -> Result<Map<String, Value>, String> {
    match argument {
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Json(object) => Ok(object.clone()),
        InvokeArgument::Text(_) => Err("a model step takes typed arguments".to_owned()),
    }
}

/// What the holder answered, as the step's outcome.
fn outcome(asked: Result<Value, String>) -> Outcome {
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

impl tilo_actions::remote::Admits for ModelSteps {
    const PROVIDER: &'static str = model_providers::PROVIDER;
    const APP: &'static str = settings_sections::APP_ID;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(steps().map(|decl| descriptor(&decl)).collect())
    }

    /// Carry out an invocation the device's dispatch admitted for `caller`.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let action = steps()
            .map(|decl| decl.id)
            .find(|action| {
                request.target.address.resource == model_providers::RESOURCE
                    && request.target.address.action == *action
            })
            .ok_or("unknown model step")?;
        // The holder answers this device as its person: an agent or an app acting for them
        // chooses no model here.
        let actor = BrokerActor::for_caller(caller);
        actor
            .direct_person()
            .ok_or("only you choose the models your workspace runs on")?;
        let workspace = caller
            .workspace()
            .ok_or("the models were asked in no workspace of this device")?;
        let workspace = WorkspaceId::parse(workspace)
            .ok_or("the models were asked in no workspace of this device")?;
        let view = self
            .view
            .as_ref()
            .ok_or("this session reaches no device daemon, so it reaches no workspace's models")?;
        let arguments = arguments(&request.argument)?;
        let target = model_providers::target(action);
        Ok(crate::landing::off_loop(view.ask_in(workspace, target, arguments)).map(outcome))
    }
}
