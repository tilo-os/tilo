//! A computer's agent filing an issue through its held device session to a
//! workspace's server, as `tilo migrate` does: the create answers the issue it
//! made, with the address it is published under, and the issue reads back
//! there — whether the issues part was already held or the write founded it.

use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::Caller;
use tilo_workspace_service::agents::{WorkspaceActionProvider, invoke_named};
use ui::tilo_resource::{Id, Resource as _};

const TASKS: &str = "so.tilo.tasks";

/// The issue a create answered, by its id and address.
fn made(answer: &Value) -> (String, String) {
    let made = answer["value"]["made"]
        .as_array()
        .unwrap_or_else(|| panic!("a create answers what it made: {answer}"));
    assert_eq!(made.len(), 1, "one issue made: {answer}");
    (
        made[0]["id"].as_str().unwrap().to_owned(),
        made[0]["address"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_through_the_device_session_answers_the_issue_it_made() {
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
    let create = |title: &str| {
        json!({
            "resource": ui::resource::chain_resource_id(&tasks::issue_list::chain()),
            "action": "create",
            "title": title,
        })
    };

    // The first issue: the issues part is not held yet, and the create founds it.
    let first = create("Plan the launch");
    let answer = invoke_named(
        &service,
        &person,
        &route,
        first["resource"].as_str().unwrap(),
        first["action"].as_str().unwrap(),
        json!({ "title": first["title"].clone() })
            .as_object()
            .unwrap()
            .clone(),
    )
    .await
    .expect("the create is made");
    let (id, address) = made(&answer);
    assert_eq!(
        address,
        format!(
            "{}/issues/{id}",
            <tasks::issue_list::IssueList as ui::tilo_resource::Resource>::KIND
        )
    );

    // A second, with the part held now: it answers its own id.
    let answer = invoke_named(
        &service,
        &person,
        &route,
        first["resource"].as_str().unwrap(),
        "create",
        json!({ "title": "Ship it" }).as_object().unwrap().clone(),
    )
    .await
    .expect("the create is made");
    let (second, _) = made(&answer);
    assert_ne!(second, id, "each create answers the issue it made");

    // Both read back where their answers said they are.
    let part = task_part(&workspaces, &workspace).await;
    let bytes = workspaces
        .resource_document(&workspace, TASKS, part.as_str())
        .await
        .unwrap()
        .unwrap();
    let issues =
        tasks::issue_list::IssueList::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
    for made in [&id, &second] {
        assert!(
            issues.issues.contains(&Id::from_raw(made.as_str())),
            "the issue answered as {made} reads back"
        );
    }
}

/// The issues part the tracker holds now, as the workspace keeps it.
async fn task_part(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> ui::resource::DocumentId {
    for index in 0..64 {
        let part = tasks::issue_list::chain().part(index);
        if workspaces
            .resource_document(workspace, TASKS, part.as_str())
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return part;
        }
    }
    panic!("the create founded no part");
}
