//! A tracker's issues listed through the action API, from the parts its
//! chain keeps them in.
//!
//! The tracker keeps its issues in numbered parts beside its own small
//! record. An issue filed through the action API the host serves is written into
//! the chain's head part, and the API lists it under the chain at once, with
//! the actions a member runs on it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{ReplicaActor, Seed};

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ))
}

#[test]
fn an_issue_filed_through_the_api_is_listed_with_what_runs_on_it() {
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(changes)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let held = crate::common::confined(
        "so.tilo.tasks",
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![changes],
        },
    )
    .expect("the tracker opens in its worker");
    held.invoke(
        &ActionAddress::new(issues(), "create"),
        serde_json::json!({"title": "Ship the launch"})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the issue is filed");
    let surveyed = held.survey_rows();
    let under: Vec<String> = surveyed
        .iter()
        .map(|row| {
            format!(
                "{} | {} | {:?}",
                row.parent_resource,
                row.resource.id,
                row.action.as_ref().map(|action| &action.address.action)
            )
        })
        .collect();
    let item_comment = surveyed.iter().any(|row| {
        row.parent_resource == issues()
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "comment")
    });
    assert!(
        item_comment,
        "the filed issue is listed with its comment: {under:#?}"
    );

    // A caller lists a level at a time, as a page and an agent's ls do: the
    // chain is a level with children, and its level lists the issue.
    let mut under: Vec<Option<String>> = vec![None];
    let mut walked = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(parent) = under.pop() {
        if !visited.insert(parent.clone()) {
            continue;
        }
        let query = tilo_actions::remote::CatalogueQuery {
            parent_resource: parent.clone(),
            max_results: 500,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        for row in held.survey_page(&query, (0, 500)).1 {
            if row.has_children {
                under.push(Some(row.resource.id.clone()));
            }
            walked.push(format!(
                "{:?} | {} | {} | {:?}",
                parent,
                row.parent_resource,
                row.resource.id,
                row.action.as_ref().map(|action| &action.address.action)
            ));
        }
        assert!(walked.len() < 5_000, "the levels end");
    }
    assert!(
        walked
            .iter()
            .any(|row| row.contains(&format!("| {} |", issues()))
                && row.ends_with("Some(\"comment\")")),
        "a level at a time lists the filed issue with its comment: {walked:#?}"
    );
}

/// The view a link to an issue opens is found where the host mints the link:
/// the issue is kept in a part, not in the tracker's own document, so its
/// view is the one the tracker's view publishes for it. Without one, the link
/// named the tracker.
#[test]
fn an_issue_in_a_part_offers_the_view_its_link_opens() {
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let made = page
        .invoke(
            &ActionAddress::new(issues(), "create"),
            serde_json::json!({"title": "Ship the launch"})
                .as_object()
                .unwrap(),
            false,
        )
        .expect("the issue is filed");
    let issue = made
        .resources
        .first()
        .expect("the create names the issue")
        .clone();
    let viewer = ui::permissions::PermissionContext::member_of(
        crate::common::WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Viewer,
    );
    let views = page.resource_views(&issue, "tasks.issues.issues", &viewer);
    assert!(
        views
            .iter()
            .any(|view| view.address.resource == issue && view.address.action == "open"),
        "the issue offers the view its link opens: {views:?}"
    );
}
