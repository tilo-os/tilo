//! A host step whose contract declares it cancellable stops when its caller cancels it, and
//! answers as cancelled: the protocol the agent's transforms use so a caller that stops waiting
//! stops the model call rather than leaving it to run, billed, to its end.

use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::services::ProcessServices;
use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use crate::workspaces::providers::{Providers, Reach};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tilo_actions::InvocationId;
use tilo_actions::remote::admitted::{offered, settled, top_level};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Outcome, Pending,
    RemoteActionTarget,
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

impl ActionProvider for Slow {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        offered(PROVIDER, APP, top_level(self.descriptors()), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request, &admitted))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

impl Slow {
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

    fn start(
        &self,
        _caller: &Caller,
        _request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let stop = Arc::clone(&admitted.stop);
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
    let providers = Providers::default();
    providers.provide(
        PROVIDER,
        APP,
        Reach::ThisDevice,
        Arc::new(Slow) as Arc<dyn ActionProvider + Send + Sync>,
    );
    let (state, _bridge_rx) = AppBrokerState::test_state(
        Some(tree.authority()),
        ProcessServices {
            providers: Some(providers),
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
