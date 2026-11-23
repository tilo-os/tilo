//! A host step whose contract declares it cancellable stops when its caller cancels it, and
//! answers as cancelled: the protocol the agent's transforms use so a caller that stops waiting
//! stops the model call rather than leaving it to run, billed, to its end.

use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::services::{HostStepOutcome, HostStepRequest, HostStepServices, ProcessServices};
use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ConfirmationPolicy,
    ExecutionLocus, ExecutionMode, InvocationUpdate, OperationKind, ResultSpec, RetryPolicy,
    RiskLevel,
};

const PROVIDER: &str = "test.cancellable";
const APP: &str = "so.tilo.agent";
const RESOURCE: &str = "agent.transform:default";
const ACTION: &str = "summarise";

/// A step that works until its caller cancels it, as a transform's model call does.
struct Slow;

impl HostStepServices for Slow {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn app_id(&self) -> &str {
        APP
    }

    fn descriptors(&self) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(RESOURCE, ACTION),
            resource_kind: "agent.transform".into(),
            resource_label: "AI transforms".into(),
            action_label: "Summarise".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: ActionKind::Undoable,
                risk: RiskLevel::Low,
                confirmation: ConfirmationPolicy::None,
                retry: RetryPolicy::Idempotent,
                execution: ExecutionMode::Background { cancellable: true },
                result: ResultSpec::Json {
                    schema: serde_json::json!({}),
                },
                ..ActionContract::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        let stop = Arc::clone(&request.stop);
        let (done, answer) = tilo_actions::remote::pending();
        drop(std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            done.complete(Err(Box::new(ActionFailure {
                code: "model_unavailable".into(),
                message: "the transform was cancelled".into(),
                details: None,
                retryable: false,
            })));
        }));
        Ok(answer)
    }
}

fn broker() -> (Arc<AppBrokerState>, String, String) {
    let owner = hex::encode([41; 32]);
    let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Cancel");
    workspace.set_owner(owner.clone());
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let (state, _bridge_rx) = AppBrokerState::test_state(
        Some(tree.authority()),
        ProcessServices {
            host_steps: vec![Arc::new(Slow)],
            ..Default::default()
        },
    );
    (state, owner, workspace_id)
}

fn summarise() -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.into(),
            address: ActionAddress::new(RESOURCE, ACTION),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The invocation's state once it is terminal, waiting on the broker's own change signal.
fn terminal(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> InvocationUpdate {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut changes = state.invocation_changes();
    loop {
        if let Some(observed) = state.action_observe(caller, &request.invocation)
            && observed.update.is_terminal()
        {
            return observed.update;
        }
        runtime
            .block_on(async {
                tokio::time::timeout(tilo_test_env::hang_bound(), changes.changed()).await
            })
            .expect("the step ends")
            .unwrap();
    }
}

/// A caller that cancels a cancellable step stops it, and the step answers as cancelled rather
/// than as failed.
#[test]
fn a_cancelled_host_step_stops_and_answers_cancelled() {
    let (state, owner, workspace) = broker();
    let person = Caller::local_person(owner).in_workspace(workspace);
    let request = summarise();
    let receipt: Result<_, ActionCallError> = state
        .action_invoke(&person, None, None, &request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once");
    receipt.expect("the person's step is admitted");

    // It is still running until cancelled.
    assert!(
        !state
            .action_observe(&person, &request.invocation)
            .expect("the step is known")
            .update
            .is_terminal(),
        "the step runs until its caller stops it"
    );
    crate::apps::collab_app_broker::answered(state.action_cancel(&person, &request.invocation))
        .expect("a cancellable step is cancelled");

    let update = terminal(&state, &person, &request);
    match update {
        InvocationUpdate::Cancelled { message, .. } => {
            assert_eq!(
                message.as_deref(),
                Some("the transform was cancelled"),
                "what the stopped step said is its cancellation's message"
            );
        }
        other => panic!("the stopped step answers as cancelled: {other:?}"),
    }
}
