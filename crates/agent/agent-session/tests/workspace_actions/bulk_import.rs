//! A tracker of 3,000 issues and their comments brought into a workspace's
//! server as `tilo migrate` brings it: issues made 200 to a `create-many`,
//! their comments as groups of the issues' own action. The workspace takes
//! every call, the last list costs about what the second did, and the
//! tracker opens with every issue afterwards.

use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::Caller;
use tilo_workspace_service::agents::{WorkspaceActionProvider, invoke_named};
use ui::tilo_resource::Resource as _;

const TASKS: &str = "so.tilo.tasks";
const ISSUES: usize = 3_000;
const LIST: usize = 200;
const COMMENTS: usize = 2;

/// The issues one `create-many` answered, by id.
fn made(answer: &Value) -> Vec<String> {
    answer["value"]["made"]
        .as_array()
        .unwrap_or_else(|| panic!("a create answers what it made: {answer}"))
        .iter()
        .map(|made| made["id"].as_str().unwrap().to_owned())
        .collect()
}

fn origin(at: usize) -> Value {
    json!({
        "author": "Grace Hopper",
        "handle": "grace@example.com",
        "source": "Linear",
        "url": format!("https://linear.example/acme/issue/ENG-{at}"),
        "reference": format!("ENG-{at}"),
        "created_ms": 1_756_720_800_000_i64,
        "updated_ms": null,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tracker_of_three_thousand_issues_lands_at_the_cost_of_its_first_lists() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
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
    let tracker = tilo_workspace_service::agents::workspace_resource(&catalogue, TASKS);
    workspaces
        .prepare_application(&workspace, &server.cert, &catalogue, TASKS, &tracker)
        .await
        .unwrap();
    let service =
        WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());
    let call = |action: &'static str, args: Value| {
        let (service, person, route, issues) = (&service, &person, &route, &issues);
        async move {
            let started = std::time::Instant::now();
            let answer = invoke_named(
                service,
                person,
                route,
                issues,
                action,
                args.as_object().unwrap().clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("the workspace takes {action}: {error:?}"));
            (answer, started.elapsed())
        }
    };

    let mut lists = Vec::new();
    let mut groups = Vec::new();
    let mut made_count = 0;
    for first in (0..ISSUES).step_by(LIST) {
        let list: Vec<Value> = (first..first + LIST)
            .map(|at| {
                json!({
                    "title": format!("Issue {at}"),
                    "body": format!("What issue {at} is about, as it was written there."),
                    "status": "todo",
                    "url": format!("https://linear.example/acme/issue/ENG-{at}"),
                    "labels": ["Migration"],
                    "origin": origin(at),
                })
            })
            .collect();
        let (answer, took) = call("create-many", json!({ "issues": list })).await;
        let ids = made(&answer);
        assert_eq!(ids.len(), LIST, "every issue made is answered: {answer}");
        made_count += ids.len();
        lists.push(took);
        let comments: Vec<Value> = ids
            .iter()
            .flat_map(|issue| {
                (0..COMMENTS).map(move |n| {
                    json!({ "action": "comment", "args": {
                        "issue": issue,
                        "text": format!("Comment {n}, moved in with its author and date."),
                        "origin": origin(n),
                    }})
                })
            })
            .collect();
        let (_, took) = call("group", json!({ "actions": comments })).await;
        groups.push(took);
    }
    println!("lists of {LIST} issues took {lists:?}; their comments {groups:?}");
    assert_eq!(made_count, ISSUES);
    // The second list lands in a tracker of 200 issues, the last in one of
    // 2,800: each costs what it makes, not what the tracker held already.
    let (early, late) = (
        lists[1] + groups[1],
        lists[lists.len() - 1] + groups[groups.len() - 1],
    );
    assert!(
        late <= early * 3,
        "the last list and its comments took {late:?}, the second {early:?}"
    );

    // The tracker opens with every issue: each list lands in the parts the
    // tracker keeps them in.
    let mut held = 0;
    for index in 0..64 {
        let part = tasks::issue_list::chain().part(index);
        let Some(bytes) = workspaces
            .resource_document(&workspace, TASKS, part.as_str())
            .await
            .unwrap()
        else {
            continue;
        };
        held += tasks::issue_list::IssueList::read(&automerge::AutoCommit::load(&bytes).unwrap())
            .unwrap()
            .issues
            .len();
    }
    assert_eq!(held, ISSUES, "every issue made opens in the tracker");
}
