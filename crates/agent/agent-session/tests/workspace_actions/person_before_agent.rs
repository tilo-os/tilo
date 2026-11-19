//! A person's request is answered while an agent fills the workspace, about as
//! fast as in a quiet one.
//!
//! An agent bringing a tracker of 2,000 issues into the workspace sends its
//! lists one after another, several at once where it has more than one
//! worker. Every request of a workspace is served on one thread: served in the
//! order they came, the person's listing of the tracker waited behind every
//! list the agents had sent before it, and timed out. The service takes a
//! person's request before an agent's that waits, and starts no agent's
//! request in the moment between two of the person's, so each of the person's
//! requests waits at most for the one agent request already running.

use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionProvider as _, Caller, InvocationState, ReceiptStatus};
use tilo_actions::{InvocationUpdate, WorkGroup};
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};

const TASKS: &str = "so.tilo.tasks";
/// What the agent brings in, in lists of `LIST`, from `STREAMS` workers at once.
const ISSUES: usize = 2_000;
const LIST: usize = 125;
const STREAMS: usize = 4;

fn listed_issues(first: usize, count: usize) -> Value {
    let list: Vec<Value> = (first..first + count)
        .map(|at| {
            json!({
                "title": format!("Imported issue {at}"),
                "body": format!("What issue {at} is about, as it was written there."),
                "status": "todo",
                "labels": ["Migration"],
            })
        })
        .collect();
    json!({ "issues": list })
}

/// `create-many` of `args` on the tracker, as an agent's `tilo mcp` on the person's computer asks
/// it: through that computer's session, as the person's device, each call a step of the agent's
/// session. Waits for it to end.
fn agent_creates(
    provider: &WorkspaceActionProvider,
    laptop: &Caller,
    route: &DocumentRoute,
    issues: &str,
    args: &Value,
) {
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: record_provider(route),
            address: ActionAddress {
                resource: issues.to_owned(),
                action: "create-many".to_owned(),
            },
        },
        argument: InvokeArgument::Json(args.as_object().unwrap().clone()),
        context: Some(tilo_actions::ActionContext {
            group: Some(WorkGroup {
                id: "agent-session:importer".into(),
                label: Some("Bring the tracker in".into()),
                kind: tilo_actions::context::AGENT_SESSION_GROUP.into(),
                declared_id: None,
            }),
            ..tilo_actions::ActionContext::default()
        }),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let invocation = request.invocation;
    let receipt = provider
        .invoke(laptop, request)
        .wait_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("the agent's list is taken in time"))
        .expect("the agent's list is answered")
        .expect("the agent's list is admitted");
    match receipt.status {
        ReceiptStatus::Completed { .. } => return,
        ReceiptStatus::Accepted => {}
        other => panic!("the agent's list is taken: {other:?}"),
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match provider
            .observe(laptop, &invocation)
            .map(|state: InvocationState| state.update)
        {
            Some(InvocationUpdate::Succeeded { .. }) => return,
            Some(InvocationUpdate::Failed { error, .. }) => {
                panic!("the agent's list failed: {}", error.message)
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "the agent's list ends");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn median(times: &mut [Duration]) -> Duration {
    times.sort();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_persons_requests_go_before_an_agents_bulk_import() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let _page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let catalogue = crate::common::catalogue();
    let tracker = tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(&workspace, &server.cert, &catalogue, TASKS, &tracker)
        .await
        .unwrap();
    let provider =
        WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
    let user = root.verifying_key().to_bytes();
    let person =
        Caller::authenticated(user, browser.cert.device_public_key).in_workspace(&workspace);
    // The agent a person starts on their laptop asks through the laptop's own session.
    let agent = Caller::authenticated(user, laptop.cert.device_public_key).in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());

    // What the person does: opens the tracker (its listing) and files one issue.
    let listing = || CatalogueQuery {
        provider: Some(record_provider(&route)),
        max_results: 64,
        ..CatalogueQuery::default()
    };
    let person_asks = || {
        let (provider, person, route, issues) = (&provider, &person, &route, &issues);
        async move {
            let started = Instant::now();
            provider
                .query(person, listing())
                .await
                .expect("the listing is answered")
                .expect("the tracker is listed");
            let listed = started.elapsed();
            let started = Instant::now();
            invoke_named(
                provider,
                person,
                route,
                issues,
                "create",
                json!({"title": "The person's own issue"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .expect("the person's issue is filed");
            (listed, started.elapsed())
        }
    };

    // Quiet: what the person's requests take with nobody else working.
    person_asks().await;
    let (mut quiet_lists, mut quiet_files) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        let (listed, filed) = person_asks().await;
        quiet_lists.push(listed);
        quiet_files.push(filed);
    }
    let (quiet_list, quiet_file) = (median(&mut quiet_lists), median(&mut quiet_files));

    // Busy: the agent's workers send their lists, one after another each.
    let import_started = Instant::now();
    let per_stream = ISSUES / STREAMS;
    let mut workers = Vec::new();
    for stream in 0..STREAMS {
        let (provider, agent, route, issues) = (
            provider.clone(),
            agent.clone(),
            route.clone(),
            issues.clone(),
        );
        workers.push(tokio::task::spawn_blocking(move || {
            let mut lists = Vec::new();
            for first in (stream * per_stream..(stream + 1) * per_stream).step_by(LIST) {
                let started = Instant::now();
                agent_creates(
                    &provider,
                    &agent,
                    &route,
                    &issues,
                    &listed_issues(first, LIST),
                );
                lists.push(started.elapsed());
            }
            lists
        }));
    }
    let (mut busy_lists, mut busy_files) = (Vec::new(), Vec::new());
    // Let the agent's lists queue before the person asks.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while workers.iter().any(|worker| !worker.is_finished()) {
        let (listed, filed) = person_asks().await;
        busy_lists.push(listed);
        busy_files.push(filed);
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let mut agent_lists = Vec::new();
    for worker in workers {
        agent_lists.extend(worker.await.unwrap());
    }
    let import = import_started.elapsed();
    let worst_list = busy_lists.iter().max().copied().unwrap_or_default();
    let worst_file = busy_files.iter().max().copied().unwrap_or_default();
    let longest_agent_list = agent_lists.iter().max().copied().unwrap_or_default();
    println!(
        "quiet: listing {quiet_list:?}, filing {quiet_file:?}; \
         during the import ({import:?}, agent lists {agent_lists:?}): \
         listing median {:?} worst {worst_list:?}, filing median {:?} worst {worst_file:?} \
         over {} asks",
        median(&mut busy_lists.clone()),
        median(&mut busy_files.clone()),
        busy_lists.len(),
    );
    assert!(
        busy_lists.len() >= 3,
        "the person asked several times while the agent worked"
    );
    // What the person waits for is at most the one agent list already running when they
    // ask, never the lists the agents sent before it: served in the order they came, the
    // listing waited for several of them.
    // Most of the person's asks find no agent list running, or one part done: a listing and
    // a filing together take less than one of the agent's lists.
    let typical_agent_list = median(&mut agent_lists.clone());
    let typical_ask = median(&mut busy_lists.clone()) + median(&mut busy_files.clone());
    assert!(
        typical_ask <= typical_agent_list,
        "the person's listing and filing took {typical_ask:?} at the median while the \
         agent's lists took {typical_agent_list:?}: they waited behind the agent's work"
    );
    let bound = longest_agent_list + quiet_list.max(quiet_file) * 4 + Duration::from_secs(1);
    assert!(
        worst_list <= bound && worst_file <= bound,
        "the person's listing took up to {worst_list:?} and filing {worst_file:?} while the \
         agent's longest list took {longest_agent_list:?}: they waited for more than the \
         list that was running"
    );
}
