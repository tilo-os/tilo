//! The machines a person planned to install, as this session keeps their
//! Servers workspace: the same steps the command types and the installer's
//! window presses, answered here.
//!
//! Nothing about a machine is kept by this session itself. The plan is its
//! entry in the person's Servers workspace ([`tilo_workspace_service::workspaces::servers`]),
//! which this device holds through its own workspace service: each step is
//! asked of that service's host operations ([`WorkspaceView::ask_own_host`]), which
//! makes the workspace when the first machine is planned and every device of
//! the person reads the one document from then on.
//!
//! The window's controls reach these steps as the app, and the command as the
//! person at this device: both are the same address
//! ([`tilo_actions::remote::machines`]).

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::{InvokeArgument, Pending, machines};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionOutput, ActionState,
    ExecutionLocus, InputSpec, OperationKind,
};
use tilo_broker_core::steps::{HostStepOutcome, HostStepRequest, HostStepServices};

use crate::workspace_view::WorkspaceView;

/// The app whose controls reach these steps: the installer's window.
const APP_ID: &str = "so.tilo.imager";

/// The machines of this session's person, answered from this device's own
/// workspace service.
pub(crate) struct MachineSteps {
    view: Option<Arc<WorkspaceView>>,
}

impl MachineSteps {
    /// The steps of a session asking the device's daemon through `view`, or of one that reaches
    /// none and so answers no plan.
    pub(crate) fn session(view: Option<Arc<WorkspaceView>>) -> Arc<Self> {
        Arc::new(Self { view })
    }

    /// The arguments the step carries, as its descriptor's parameters take
    /// them.
    fn arguments(request: &HostStepRequest<'_>) -> Result<Map<String, Value>, String> {
        match InvokeArgument::from_slot(request.argument) {
            InvokeArgument::None => Ok(Map::new()),
            InvokeArgument::Json(object) => Ok(object),
            InvokeArgument::Text(_) => Err("a machine step takes typed arguments".to_owned()),
        }
    }

    /// Ask this device's own workspace service the host step `action` with
    /// `arguments`: the service answers every step from the Servers workspace
    /// it holds. Planning the person's first machine founds that workspace
    /// there; a plan made where another device of theirs keeps it is refused,
    /// and the workspace reaches this device from where it is kept.
    fn ask(
        &self,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Pending<Result<Value, String>>, String> {
        let asked = self
            .view
            .as_ref()
            .ok_or("this session reaches no device daemon, so it holds no Servers workspace")?
            .ask_own_host(action, arguments)
            .ok_or("this device's workspace service is not open")?;
        Ok(crate::landing::off_loop(asked))
    }
}

/// The step `request` names, described for the broker's catalogue.
fn descriptor(step: &str) -> ActionDescriptor {
    let (label, kind) = match step {
        machines::PLAN => ("Plan a machine", OperationKind::Effect),
        machines::LIST => ("List the machines you planned", OperationKind::View),
        machines::SHOW => ("Show a machine you planned", OperationKind::View),
        machines::SERVERS => ("List your servers", OperationKind::View),
        machines::RECOVERY_KEYS => (
            "Read your machines' disk recovery keys",
            OperationKind::View,
        ),
        _ => ("Take a planned machine in", OperationKind::Effect),
    };
    let contract = if kind == OperationKind::View {
        ActionContract::view()
    } else {
        ActionContract::default()
    };
    ActionDescriptor {
        did: None,
        state: ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(machines::RESOURCE, step),
        resource_kind: machines::RESOURCE.into(),
        resource_label: "Machines".into(),
        action_label: label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::default(),
        kind,
        contract,
        input: InputSpec::None,
        params: machines::params(step),
        view: None,
    }
}

/// What asking the service answered, as the step's outcome.
fn outcome(asked: Result<Value, String>) -> HostStepOutcome {
    asked
        .map(|value| ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
        .map_err(|message| {
            Box::new(ActionFailure {
                code: "machine_refused".into(),
                message,
                details: None,
                retryable: false,
            })
        })
}

impl HostStepServices for MachineSteps {
    fn provider(&self) -> &'static str {
        machines::PROVIDER
    }

    fn app_id(&self) -> &str {
        APP_ID
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        machines::STEPS.into_iter().map(descriptor).collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let step = machines::STEPS
            .into_iter()
            .find(|step| {
                request.address.resource == machines::RESOURCE && request.address.action == *step
            })
            .ok_or("unknown machine step")?;
        // A disk's recovery key is the person's alone: the installer's window plans machines,
        // and neither it nor an agent reads what opens their disks.
        if step == machines::RECOVERY_KEYS && (request.actor.is_app() || request.actor.is_agent()) {
            return Err("only you read your machines' recovery keys, on a device of yours".into());
        }
        let arguments = Self::arguments(request)?;
        Ok(self.ask(step, arguments)?.map(outcome))
    }
}
