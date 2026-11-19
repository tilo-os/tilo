//! A workspace's server restarted with a tracker whose issues fill several parts of its chain.
//!
//! The server's workspace actions join the workspace again from nothing: they are sent the
//! newest part of the issues' chain first, and the earlier parts only when asked for them. A
//! write naming the oldest issue, which lives in the first part, must not wait for another write
//! to bring that part: the service asks for every part the tracker lacks as soon as it opens it,
//! so the write lands within a bound.

use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::Caller;
use tilo_server::workspace_agents::{WorkspaceActionProvider, invoke_named};

const TASKS: &str = "so.tilo.tasks";
/// A part holds 1,000 issues: these fill three.
const ISSUES: usize = 2_200;
const LIST: usize = 200;
/// How long after the restart a write naming the oldest issue may take to land.
const BOUND: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_server_holds_every_part_and_writes_under_the_oldest_issue() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let catalogue = crate::common::catalogue();
    let tracker = tilo_server::workspace_bots::workspace_resource(&catalogue, TASKS);
    let issues = ui::resource::chain_resource_id(&tasks::issue_list::chain());
    let person = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    );

    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let person = person.in_workspace(&workspace);
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.clone(),
        document: tracker.clone(),
    };
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    workspaces
        .prepare_application(&workspace, &server.cert, &catalogue, TASKS, &tracker)
        .await
        .unwrap();
    let mut oldest = None;
    {
        let service =
            WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
        for first in (0..ISSUES).step_by(LIST) {
            let list: Vec<Value> = (first..first + LIST)
                .map(|at| json!({ "title": format!("Issue {at}"), "body": "What it is about." }))
                .collect();
            let answer = invoke_named(
                &service,
                &person,
                &route,
                &issues,
                "create-many",
                json!({ "issues": list }).as_object().unwrap().clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("the workspace takes create-many: {error:?}"));
            let made = answer["value"]["made"]
                .as_array()
                .unwrap_or_else(|| panic!("a create answers what it made: {answer}"));
            assert_eq!(made.len(), LIST, "{answer}");
            oldest.get_or_insert_with(|| made[0]["id"].as_str().unwrap().to_owned());
        }
    }
    let oldest = oldest.unwrap();
    let mut parts = 0;
    for index in 0..64 {
        let part = tasks::issue_list::chain().part(index);
        if workspaces
            .resource_document(&workspace, TASKS, part.as_str())
            .await
            .unwrap()
            .is_some()
        {
            parts += 1;
        }
    }
    assert!(parts >= 3, "the issues fill {parts} parts of their chain");

    // The server restarts: its workspace actions keep nothing of what they were sent.
    drop(page);
    drop(actions);
    served.shutdown().await;
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &server,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let service =
        WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
    let started = std::time::Instant::now();
    let mut last = None;
    while started.elapsed() < BOUND {
        match invoke_named(
            &service,
            &person,
            &route,
            &issues,
            "create",
            json!({ "title": "Under the oldest", "parent": oldest })
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        {
            Ok(answer) => {
                println!(
                    "a write under the oldest of {parts} parts landed {:?} after the restart",
                    started.elapsed()
                );
                assert!(answer["value"].is_object(), "{answer}");
                drop(served);
                return;
            }
            Err(error) => last = Some(format!("{error:?}")),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!(
        "a write under the oldest issue did not land within {BOUND:?} of the restart: {}",
        last.unwrap_or_default()
    );
}
