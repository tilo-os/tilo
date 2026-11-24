//! Where an offloadable invocation runs, asked of this daemon by the session's broker
//! ([`tilo_actions::remote::offload_place`]).
//!
//! The session's broker admits an app's offloadable invocation, then asks this device's daemon to
//! place its turn on the workspace's other devices: the daemon holds the endpoint and the link to
//! them, while the session does not. The daemon answers another device's output, or says the
//! invocation should run here, where the session's broker hands it to the app's own handler. With
//! offload turned off here, every invocation runs here.

use std::sync::Arc;

use tilo_actions::offload::{ActionOffload, OffloadRequest, Offloaded};
use tilo_actions::remote::admitted::top_level;
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending, offload_place,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionKind, ActionOutput, ExecutionLocus,
    OperationKind, ResultSpec,
};

/// The device's offload route, answered for the session's broker.
pub(crate) struct Placement {
    /// The route this device's own broker asks, or `None` when offload is turned off here.
    route: Option<Arc<dyn ActionOffload>>,
    runtime: tokio::runtime::Handle,
}

impl Placement {
    /// The placement `route` answers for, as this device's broker asks it.
    pub(crate) fn new(route: Option<Arc<dyn ActionOffload>>) -> Self {
        Self {
            route,
            runtime: tilo_rt::app().handle().clone(),
        }
    }

    /// What became of `ask`: another device's output, or why it runs here.
    async fn place(
        route: Arc<dyn ActionOffload>,
        ask: offload_place::Ask,
    ) -> offload_place::Placed {
        let request = OffloadRequest {
            workspace: ask.workspace.clone(),
            app_id: ask.app_id.clone(),
            address: ActionAddress::new(ask.resource.clone(), ask.action.clone()),
            offload: ask.offload.clone(),
            inputs: ask.inputs.clone(),
        };
        // The invocation the app made, which the device that takes the turn runs.
        let invoked = InvokeRequest {
            on_behalf_of: None,
            invocation: ask.invocation,
            target: offload_place::target(),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        match route.offload_invocation(request, invoked).await {
            Offloaded::Ran {
                output,
                device,
                elapsed,
            } => offload_place::Placed::Ran {
                output,
                device,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            },
            Offloaded::RunHere { reason } => offload_place::Placed::Here { reason },
        }
    }
}

/// What a placement answers.
fn answered(placed: offload_place::Placed) -> Outcome {
    match serde_json::to_value(placed) {
        Ok(value) => Ok(ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        }),
        Err(error) => Err(Box::new(tilo_actions::ActionFailure {
            code: "offload_refused".into(),
            message: format!("the placement does not encode: {error}"),
            details: None,
            retryable: false,
        })),
    }
}

impl Admits for Placement {
    const PROVIDER: &'static str = offload_place::PROVIDER;
    const APP: &'static str = offload_place::PROVIDER;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        top_level(vec![descriptor()])
    }

    fn start(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let InvokeArgument::Json(argument) = &request.argument else {
            return Err("a placement asks with its arguments as an object".to_owned());
        };
        let ask: offload_place::Ask =
            serde_json::from_value(serde_json::Value::Object(argument.clone()))
                .map_err(|error| format!("the placement does not read: {error}"))?;
        let Some(route) = self.route.clone() else {
            return Ok(Pending::ready(answered(offload_place::Placed::Here {
                reason: "offload is turned off on this device".to_owned(),
            })));
        };
        let (done, pending) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let placed = Self::place(route, ask).await;
            done.complete(answered(placed));
        });
        Ok(pending)
    }
}

/// The placement action, as a catalogue lists it.
fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: offload_place::target().address,
        resource_kind: offload_place::RESOURCE.to_owned(),
        resource_label: "Offload".to_owned(),
        action_label: "Place".to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(
            "Run an offloadable invocation on the device that takes its turn, or here.".to_owned(),
        ),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            result: ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            },
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
        effects: Vec::new(),
    }
}
