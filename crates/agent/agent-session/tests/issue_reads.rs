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

/// A tracker listed again by a member whose access was read again since, unchanged, lists
/// without laying its view out or building its tree: the workspace reads a caller's access again
/// every half minute, and taking the same access in again laid the view out and built its whole
/// tree, which for a tracker of thousands of issues cost the next listing most of its time.
#[test]
fn a_tracker_listed_again_after_its_access_is_read_again_reuses_its_tree() {
    let mut made = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let actions: Vec<serde_json::Value> = (0..20)
        .map(|n| serde_json::json!({"action": "create", "args": {"title": format!("Issue {n}")}}))
        .collect();
    made.invoke(
        &ActionAddress::new(issues(), "group"),
        serde_json::json!({"actions": actions}).as_object().unwrap(),
        false,
    )
    .expect("the issues are filed");
    let Some(DocumentMutation::Snapshot(tracker)) = made.take_mutation().unwrap() else {
        panic!("a new tracker is published whole");
    };
    let parts = made.take_document_mutations().unwrap();
    let access = ui::permissions::PermissionContext::member_of(
        crate::common::WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Owner,
    )
    .access()
    .clone()
    .as_acting(
        tilo_action_policy::Acting::agent("Claude Code")
            .acting_for(tilo_action_policy::Acting::person("ada"))
            .in_session("s1"),
    )
    .unwrap()
    .granted(tilo_action_policy::roles::certified_agent(
        crate::common::WORKSPACE,
        "Claude Code",
        "ada",
    ));
    let member = |unix_time| ReplicaActor::Member {
        access: access.clone(),
        sandbox: Vec::new(),
        unix_time,
        agent_name: None,
        device: None,
    };
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        member(1_900_000_000),
        Seed::Existing {
            record: made.document_id().clone(),
            changes: vec![tracker],
        },
    )
    .unwrap();
    for (id, part) in &parts {
        page.apply_document(id, part).unwrap();
    }
    let listing = tilo_actions::remote::CatalogueQuery {
        parent_resource: Some(issues()),
        max_results: 2000,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let ((first, _), built_first) =
        ui::tilo_resource::profiling::observe(|| page.survey_page(&listing, (false, (0, 2000))));
    assert!(first >= 20, "every issue is listed: {first} rows");
    assert_eq!(
        built(&built_first).0,
        0,
        "the first listing walks the chain's level, not the tree the view publishes"
    );

    // The workspace reads the member's access again a minute later: nothing of it moved.
    page.renew(member(1_900_000_060)).unwrap();
    let (again, observed) =
        ui::tilo_resource::profiling::observe(|| page.survey_page(&listing, (false, (0, 2000))).0);
    assert_eq!(again, first, "the listing lists the same issues");
    assert_eq!(
        built(&observed),
        (0, 0, 0),
        "the tracker's view was laid out and its tree built again for access that did not move"
    );
}

/// A tracker held for a member whose workspace changes around them: another person admitted
/// leaves what the member may do as it was, so their view keeps what it laid out; the member
/// demoted to a viewer, then removed, may do less each time, so their view is laid out again
/// under what they hold now and refuses what they lost.
#[test]
fn a_trackers_view_is_laid_out_again_only_when_its_members_own_access_moves() {
    const OWNER: &str = "0wner";
    const ADA: &str = "ada";
    let cx = tilo_action_policy::Context::at_secs(1_900_000_000);
    let mut policy = tilo_action_policy::WorkspacePolicy::new(crate::common::WORKSPACE, OWNER);
    let govern = |policy: &mut tilo_action_policy::WorkspacePolicy, change| {
        let before = policy.clone();
        let owner = before.person_on_device(OWNER, false);
        policy
            .apply(OWNER, &owner, change, &cx, true)
            .expect("the owner governs the workspace");
    };
    govern(
        &mut policy,
        tilo_action_policy::Change::AddMember {
            person: ADA.into(),
            role: tilo_action_policy::Role::Editor,
        },
    );

    let mut made = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person(ADA),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let actions: Vec<serde_json::Value> = (0..20)
        .map(|n| serde_json::json!({"action": "create", "args": {"title": format!("Issue {n}")}}))
        .collect();
    made.invoke(
        &ActionAddress::new(issues(), "group"),
        serde_json::json!({"actions": actions}).as_object().unwrap(),
        false,
    )
    .expect("the issues are filed");
    let Some(DocumentMutation::Snapshot(tracker)) = made.take_mutation().unwrap() else {
        panic!("a new tracker is published whole");
    };
    let parts = made.take_document_mutations().unwrap();

    let member = |policy: &tilo_action_policy::WorkspacePolicy, unix_time| ReplicaActor::Member {
        access: tilo_action_policy::MemberAccess::person(policy.clone(), ADA)
            .as_acting(
                tilo_action_policy::Acting::agent("Claude Code")
                    .acting_for(tilo_action_policy::Acting::person(ADA))
                    .in_session("s1"),
            )
            .unwrap()
            .granted(tilo_action_policy::roles::certified_agent(
                &policy.workspace,
                "Claude Code",
                ADA,
            )),
        sandbox: Vec::new(),
        unix_time,
        agent_name: None,
        device: None,
    };
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person(ADA),
        member(&policy, 1_900_000_000),
        Seed::Existing {
            record: made.document_id().clone(),
            changes: vec![tracker],
        },
    )
    .unwrap();
    for (id, part) in &parts {
        page.apply_document(id, part).unwrap();
    }
    let listing = tilo_actions::remote::CatalogueQuery {
        parent_resource: Some(issues()),
        max_results: 2000,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let (first, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert!(first >= 20, "every issue is listed: {first} rows");
    let file = |page: &mut tilo_agent_session::Replica, title: &str| {
        page.invoke(
            &ActionAddress::new(issues(), "create"),
            serde_json::json!({"title": title}).as_object().unwrap(),
            false,
        )
    };
    file(&mut page, "Filed as an editor").expect("an editor files an issue");
    // The times the view is laid out from a renew of ada's access to the next read of the
    // tree it publishes, which lays out whatever it was left to lay out. What the view had
    // left from before is laid out first, so only the renew is counted.
    let renewed = |page: &mut tilo_agent_session::Replica, actor| {
        let _laid_out = page.resources();
        let (_, observed) = ui::tilo_resource::profiling::observe(|| {
            page.renew(actor).unwrap();
            page.resources()
        });
        built(&observed).1
    };

    // Someone else is admitted: what ada may do is as it was.
    govern(
        &mut policy,
        tilo_action_policy::Change::AddMember {
            person: "bea".into(),
            role: tilo_action_policy::Role::Editor,
        },
    );
    let (as_editor, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert_eq!(
        renewed(&mut page, member(&policy, 1_900_000_030)),
        0,
        "ada's view was laid out again for another person's admission"
    );
    let (again, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert_eq!(
        again, as_editor,
        "the listing lists what it did, issues and actions"
    );
    file(&mut page, "Filed after bea joined").expect("ada still files issues");

    // Ada is made a viewer: the view is laid out under that, and filing is refused.
    govern(
        &mut policy,
        tilo_action_policy::Change::SetRole {
            person: ADA.into(),
            role: tilo_action_policy::Role::Viewer,
        },
    );
    assert!(
        renewed(&mut page, member(&policy, 1_900_000_060)) > 0,
        "ada's view kept what it laid out for an editor after being made a viewer"
    );
    let (as_viewer, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert!(
        as_viewer >= 22,
        "a viewer still lists the tracker's issues: {as_viewer} rows"
    );
    assert!(
        file(&mut page, "Filed as a viewer").is_err(),
        "a viewer files no issue"
    );

    // Ada is removed: the view is laid out again, and ada reads nothing of the tracker.
    govern(
        &mut policy,
        tilo_action_policy::Change::RemoveMember { person: ADA.into() },
    );
    assert!(
        renewed(&mut page, member(&policy, 1_900_000_090)) > 0,
        "ada's view kept what it laid out for a viewer after being removed"
    );
    let (removed, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert_eq!(
        removed, 0,
        "a removed member lists none of the tracker's issues"
    );
    assert!(
        file(&mut page, "Filed after removal").is_err(),
        "a removed member files no issue"
    );
}
