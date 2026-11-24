//! The transforms' places belong to each agent the device answers for, and a transform a caller
//! cancels stops its model call and ends as cancelled through the broker.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tilo_action_policy::MemberId;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use super::*;

/// The resource the transforms are actions of, as the agent publishes them.
const RESOURCE: &str = "agent.transform:default";
use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::services::ProcessServices;
use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use crate::work::AgentEngine;
use crate::workspaces::fixtures::{governor, test_host};
use crate::workspaces::governed;
use crate::workspaces::model_answers::scripted::{Linking, Reply, ScriptedProvider};

/// An agent engine that acts as every agent: what makes this device one that answers its
/// workspaces' agents, and so runs their transforms.
struct Keys;

impl AgentEngine for Keys {
    fn acts_as<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(std::future::ready(true))
    }
}

/// Two agents on one device each run four transforms at once; the fifth one agent asks is
/// refused as busy, while the other's four go on.
#[test]
fn two_agents_each_run_four_transforms_and_a_fifth_is_refused_for_that_agent() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let transforms = Transforms::new(ServiceSlot::default(), runtime.handle().clone());
    let (scout, tide) = (MemberId::agent("scout"), MemberId::agent("tide"));

    let mut scouts = Vec::new();
    for _ in 0..transform::MAX_RUNNING {
        scouts.push(
            transforms
                .running
                .begin(&scout)
                .expect("the agent has a place free"),
        );
    }
    let mut tides = Vec::new();
    for _ in 0..transform::MAX_RUNNING {
        tides.push(
            transforms
                .running
                .begin(&tide)
                .expect("another agent has its own places"),
        );
    }
    assert_eq!(
        tides.len(),
        transform::MAX_RUNNING,
        "the other agent runs four too"
    );
    assert!(
        transforms.running.begin(&scout).is_none(),
        "the fifth transform one agent asks is refused, though the other agent still runs four"
    );
    drop(scouts.pop());
    assert!(
        transforms.running.begin(&scout).is_some(),
        "a place is free once one of that agent's transforms ends"
    );
    assert!(
        transforms.running.begin(&tide).is_none(),
        "one agent's transforms ending free nothing of another's"
    );
}

/// A transform cancelled while its model answers stops the call, asks nothing again and reads
/// cancelled in the broker's own state.
#[test]
fn a_transform_cancelled_while_it_runs_stops_and_reads_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server, workspace) = runtime.block_on(async {
        let server = test_host(dir.path(), &key, &owner).await;
        let (workspace, _, _service) = governed(server.workspaces()).await;
        server.workspaces().run_agents(Arc::new(Keys));
        server.workspaces().call_private_model_addresses(true);
        server
            .workspaces()
            .link_model_for(
                &workspace,
                &Linking {
                    name: "Scripted",
                    protocol: agent_provider_config::ProviderProtocol::OpenAiCompatible,
                    endpoint: "https://models.example.com/v1",
                    key: Some("sk-not-a-real-key"),
                    models: &["listed-model"],
                },
                true,
            )
            .await
            .expect("the workspace's one model is linked");
        (server, workspace)
    });
    // An answer long enough that the run is still writing when the caller cancels it.
    let long = "word ".repeat(200);
    let scripted =
        ScriptedProvider::answering([Reply::text(&long).pausing(Duration::from_millis(10))]);
    server.workspaces().answer_models_with(scripted.clone());

    // The broker names the workspace the service answers in, and its owner, and the device's
    // own transforms are registered on this device.
    let slot = ServiceSlot::default();
    slot.install(server.shared());
    let owner_hex = hex::encode(owner.user_public_key);
    let mut ui_workspace = Workspace::new(WorkspaceId::parse(&workspace).unwrap(), "Transforms");
    ui_workspace.policy.set_owner(owner_hex.clone());
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(ui_workspace);
    let tree = tree.shared();
    let providers = crate::workspaces::providers::Providers::default();
    Arc::new(Transforms::new(slot, runtime.handle().clone())).register(&providers);
    let state = AppBrokerState::test_state_only(
        Some(tree.authority()),
        ProcessServices {
            providers: Some(providers),
            ..tree.services()
        },
    );
    let person = Caller::local_person(owner_hex).in_workspace(workspace);

    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.into(),
            address: ActionAddress::new(RESOURCE, "summarise"),
        },
        argument: InvokeArgument::Json(
            json!({"input": "A page that says what a transform is."})
                .as_object()
                .unwrap()
                .clone(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt: Result<_, ActionCallError> = state
        .action_invoke(&person, None, None, &request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once");
    receipt.expect("the person's transform is admitted");

    // The model is answering when the person cancels the transform.
    runtime.block_on(async {
        tilo_test_env::within("the transform's model call starts", async {
            while scripted.asked().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    });
    crate::apps::collab_app_broker::answered(state.action_cancel(&person, &request.invocation))
        .expect("a cancellable transform is cancelled");

    let update = terminal(&state, &person, &request);
    let InvocationUpdate::Cancelled { message, .. } = update else {
        panic!("the stopped transform answers as cancelled: {update:?}");
    };
    assert!(
        message.is_some_and(|said| !said.trim().is_empty()),
        "the stopped run says why it stopped"
    );
    assert_eq!(
        scripted.asked().len(),
        1,
        "the run stopped on its first answer: no repair attempt was asked"
    );
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
