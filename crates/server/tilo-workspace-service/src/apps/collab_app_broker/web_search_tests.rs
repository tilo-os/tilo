//! A shortcut's step and an app reach the public web through the broker's action api: the
//! `web.search:default` service answers `search` and `fetch` as this device's own host step.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::agents::WebSearch;
use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::services::ProcessServices;
use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
use crate::workspaces::providers::Providers;
use std::sync::Arc;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationUpdate};
use tilo_web_search::Engine;
use tilo_web_search::fixtures::{FixtureServer, Route};

const BRAVE_RESULTS: &str =
    include_str!("../../../../../integrations/tilo-web-search/tests/fixtures/brave.html");
const BING_RESULTS: &str =
    include_str!("../../../../../integrations/tilo-web-search/tests/fixtures/bing.html");

fn broker(web: WebSearch) -> (Arc<AppBrokerState>, String, String) {
    let owner = hex::encode([41; 32]);
    let mut workspace = Workspace::new(WorkspaceId::from_u128(9), "Web");
    workspace.set_owner(owner.clone());
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let providers = Providers::default();
    Arc::new(web).register(&providers);
    let (state, _bridge_rx) = AppBrokerState::test_state(
        Some(tree.authority()),
        ProcessServices {
            providers: Some(providers),
            ..Default::default()
        },
    );
    (state, owner, workspace_id)
}

fn search(arguments: serde_json::Map<String, serde_json::Value>) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: crate::agents::WEB_SEARCH_PROVIDER.into(),
            address: ActionAddress::new("web.search:default", "search"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn invoked(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
    state
        .action_invoke(caller, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}

/// The invocation's state once `until` holds of it, waiting on the broker's own change signal.
fn observed(
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

/// A shortcut's `search` step is the web.search action: the engines are asked the query and
/// their merged results are what the step answers.
#[test]
fn a_shortcut_step_searches_the_web_through_its_action() {
    let fixtures = FixtureServer::serve(vec![
        Route::new("/brave", "text/html", BRAVE_RESULTS),
        Route::new("/bing", "text/html", BING_RESULTS),
    ]);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let web = WebSearch::new(runtime.handle().clone()).looking_at(vec![
        (Engine::Brave, fixtures.at("/brave")),
        (Engine::Bing, fixtures.at("/bing")),
    ]);
    let (state, owner, workspace) = broker(web);
    let person = Caller::local_person(owner).in_workspace(workspace);

    let mut arguments = serde_json::Map::new();
    arguments.insert("query".into(), serde_json::json!("rust borrow checker"));
    let request = search(arguments);
    invoked(&state, &person, &request).expect("the person's step is admitted");

    let update = observed(&state, &person, &request);
    let InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("the search answered: {update:?}");
    };
    let value = output.value.expect("the step answered a value");
    assert_eq!(value["safe_search"], "off");
    let results = value["results"]
        .as_array()
        .expect("the answer holds results");
    assert!(!results.is_empty(), "{value}");
    assert!(
        value["total_results"].as_u64().unwrap_or_default() >= 3,
        "{value}"
    );
    assert!(
        results
            .iter()
            .any(|result| result["title"] == "Rust Programming Language"),
        "{value}"
    );
    let seen = fixtures.requests();
    assert!(
        seen.iter()
            .any(|request| request.parameter("q").as_deref() == Some("rust borrow checker")),
        "the engines were asked the query: {seen:?}"
    );
}
