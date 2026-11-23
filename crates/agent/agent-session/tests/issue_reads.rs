//! A tracker's issues listed through the action API, from the parts its
//! chain keeps them in.
//!
//! The tracker keeps its issues in numbered parts beside its own small
//! resource. An issue filed through the action API the host serves is written into
//! the chain's head part, and the API lists it under the chain at once, with
//! the actions a member runs on it.

use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{ReplicaActor, Seed};

/// The place in the tracker of an issue that stands past the page of issues its part publishes:
/// the part holds it, the chain names it, and no published tree carries it.
const PAST_PAGE: usize = 400;

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

/// Every issue `page` keeps, by the id a path names it by, in the order its
/// parts hold them: an issue at 300 or more within its part stands past the
/// page that part publishes.
fn held_issues(page: &tilo_agent_session::Replica) -> Vec<String> {
    use ui::tilo_resource::Resource as _;
    let mut held = Vec::new();
    for index in 0..16 {
        let Some(Ok(part)) = page.read_resource(&tasks::issue_list::chain().part(index), |doc| {
            tasks::issue_list::IssueList::read(doc)
        }) else {
            break;
        };
        held.extend(part.issues.iter().map(|(id, _)| id.as_str().to_owned()));
    }
    held
}

/// The times `observed` built the whole tree a view publishes, laid the view
/// out, and read a whole part to apply one action to it.
fn built(observed: &[ui::tilo_resource::profiling::Sample]) -> (usize, usize, usize) {
    let calls = |name: &str| {
        observed
            .iter()
            .filter(|phase| phase.phase == name)
            .map(|phase| phase.calls)
            .sum::<usize>()
    };
    (
        calls("framework.published_tree"),
        calls("ui.layout"),
        calls("resource.apply_whole_read"),
    )
}

/// An issue named by its id, as an agent's path names one and acts on it:
/// the chain a path starts with named at the app's own level, the issue found
/// under it, its action looked up by address, renamed and deleted, in a
/// replica opened afresh over what the workspace holds, as the device serving
/// the workspace opens one for a call. None of it builds the tree the tracker publishes or lays its
/// view out, at twenty-five issues as at a thousand, and an issue past the
/// page its part publishes is found and acted on the same way.
#[test]
fn an_issue_named_by_its_id_is_acted_on_without_building_the_tree() {
    for size in [25, 1000] {
        let mut page = crate::common::open(
            "so.tilo.tasks",
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
        )
        .unwrap();
        let group = ActionAddress::new(issues(), "group");
        for start in (0..size).step_by(tasks::model::MAX_MANY) {
            let actions: Vec<serde_json::Value> = (start
                ..(start + tasks::model::MAX_MANY).min(size))
                .map(|n| {
                    serde_json::json!({"action": "create", "args": {"title": format!("Issue {n}")}})
                })
                .collect();
            page.invoke(
                &group,
                serde_json::json!({"actions": actions}).as_object().unwrap(),
                false,
            )
            .expect("the issues are filed");
        }
        let held = held_issues(&page);
        assert_eq!(held.len(), size, "the tracker keeps every issue filed");
        let Some(DocumentMutation::Snapshot(tracker)) = page.take_mutation().unwrap() else {
            panic!("a new tracker is published whole");
        };
        let parts = page.take_document_mutations().unwrap();
        let mut targets = vec![held[100.min(size - 1)].clone()];
        if size > PAST_PAGE {
            targets.push(held[PAST_PAGE].clone());
        }
        // A path names the chain the app keeps at its own level
        // (`tasks/Issues/<id>`) before the item: the app answers the name
        // from the chain's head part, never a page of the listing built from
        // every item the app shows.
        let named_chain = tilo_actions::remote::CatalogueQuery {
            parent_resource: None,
            resource_name: Some("Issues".to_owned()),
            max_results: 50,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        for target in targets {
            let mut fresh = crate::common::open(
                "so.tilo.tasks",
                &MemberId::person("ada"),
                ReplicaActor::Owner,
                Seed::Existing {
                    record: page.document_id().clone(),
                    changes: vec![tracker.clone()],
                },
            )
            .unwrap();
            for (id, part) in &parts {
                fresh.apply_document(id, part).unwrap();
            }
            let named = tilo_actions::remote::CatalogueQuery {
                parent_resource: Some(issues()),
                resource_name: Some(target.clone()),
                max_results: 50,
                ..tilo_actions::remote::CatalogueQuery::default()
            };
            let (rows, observed) = ui::tilo_resource::profiling::observe(|| {
                let (_, chain) = fresh.survey_page(&named_chain, (false, (0, 50)));
                assert!(
                    chain.iter().any(|row| row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.address.action == "create")),
                    "the app's level names the chain a path starts with, at {size} issues: {chain:#?}"
                );
                let (_, rows) = fresh.survey_page(&named, (false, (0, 50)));
                let item = rows.iter().find_map(|row| row.action.as_ref()).map_or_else(
                    || panic!("{target} is found under the chain"),
                    |action| action.address.resource.clone(),
                );
                for (action, arguments, confirmed) in [
                    (
                        "rename",
                        serde_json::json!({"title": "Renamed by id"}),
                        false,
                    ),
                    ("delete", serde_json::json!({}), true),
                ] {
                    let address = ActionAddress::new(item.as_str(), action);
                    let target_query = tilo_actions::remote::CatalogueQuery {
                        target: Some(address.clone()),
                        max_results: 1,
                        ..tilo_actions::remote::CatalogueQuery::default()
                    };
                    let (_, found) = fresh.survey_page(&target_query, (false, (0, 1)));
                    assert_eq!(found.len(), 1, "{action} is found by its address");
                    assert!(fresh.descriptor(&address).is_some());
                    fresh
                        .invoke(&address, arguments.as_object().unwrap(), confirmed)
                        .unwrap_or_else(|error| panic!("{action} of {target}: {error}"));
                }
                rows
            });
            assert!(!rows.is_empty());
            assert_eq!(
                built(&observed),
                (0, 0, 0),
                "{target} of {size} issues was acted on by building the tree, laying it out or reading its whole part"
            );
            assert!(
                !held_issues(&fresh).contains(&target),
                "{target} of {size} issues is deleted"
            );
        }
    }
}
