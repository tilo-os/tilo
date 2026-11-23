//! A chat's messages listed through the action API, from the parts its chain
//! keeps them in.
//!
//! The chat keeps its messages in numbered parts beside its own small
//! resource. A message posted through the action API the host serves is written
//! into the chain's head part, and the API lists it under the chain at once,
//! with the actions a member runs on it.

use collab_types::DocumentMutation;
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{ReplicaActor, Seed};

/// The place in the chat of a message that stands past the page of messages its part publishes:
/// the part holds it, the chain names it, and no published tree carries it.
const PAST_PAGE: usize = 400;

/// The chat's channel the posts go in.
const CHANNEL: &str = "c_chat";

/// The address the chat publishes its messages under.
fn messages() -> String {
    ui::resource::chain_resource_id(&chat::parts::chain())
}

/// A chat a page founded, with the channel the posts go in.
fn founded() -> tilo_agent_session::Replica {
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    page.invoke(
        &ActionAddress::new("chat.workspace", "create-channel-with-id"),
        serde_json::json!({"id": CHANNEL, "name": "chat", "category": null,
            "kind": null, "topic": null, "members": null})
        .as_object()
        .unwrap(),
        false,
    )
    .expect("the chat takes a channel");
    page
}

/// One posted message, as the chain's `post` takes it.
fn posted(index: usize) -> serde_json::Value {
    serde_json::json!({
        "action": "post",
        "args": {"channel": CHANNEL, "text": format!("Message {index}: a line of chat")},
    })
}

/// Post messages `from..to`, as one change through the chain's own `group`.
fn post(page: &mut tilo_agent_session::Replica, from: usize, to: usize) {
    let posts: Vec<serde_json::Value> = (from..to).map(posted).collect();
    page.invoke(
        &ActionAddress::new(messages(), "group"),
        serde_json::json!({ "actions": posts }).as_object().unwrap(),
        false,
    )
    .expect("the messages are posted");
}

#[test]
fn a_message_posted_through_the_api_is_listed_with_what_runs_on_it() {
    let mut page = founded();
    let Some(DocumentMutation::Snapshot(changes)) = page.take_mutation().unwrap() else {
        panic!("a new record is published whole");
    };
    let held = crate::common::confined(
        "so.tilo.chat",
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![changes],
        },
    )
    .expect("the chat opens in its worker");
    held.invoke(
        &ActionAddress::new(messages(), "post"),
        serde_json::json!({"channel": CHANNEL, "text": "Ship the launch"})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the message is posted");
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
    let item_edit = surveyed.iter().any(|row| {
        row.parent_resource == messages()
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "edit-message")
    });
    assert!(
        item_edit,
        "the posted message is listed with its edit: {under:#?}"
    );

    // A caller lists a level at a time, as a page and an agent's ls do: the
    // chain is a level with children, and its level lists the message.
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
            .any(|row| row.contains(&format!("| {} |", messages()))
                && row.ends_with("Some(\"edit-message\")")),
        "a level at a time lists the posted message with its edit: {walked:#?}"
    );
}

/// The view a link to a message opens is found where the host mints the link:
/// the message is kept in a part, not in the chat's own document, so its
/// view is the one the chat's view publishes for it. Without one, the link
/// named the chat.
#[test]
fn a_message_in_a_part_offers_the_view_its_link_opens() {
    let mut page = founded();
    let made = page
        .invoke(
            &ActionAddress::new(messages(), "post"),
            serde_json::json!({"channel": CHANNEL, "text": "Ship the launch"})
                .as_object()
                .unwrap(),
            false,
        )
        .expect("the message is posted");
    let message = made
        .resources
        .first()
        .expect("the post names the message")
        .clone();
    let viewer = ui::permissions::PermissionContext::member_of(
        crate::common::WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Viewer,
    );
    let views = page.resource_views(&message, "chat.messages.messages", &viewer);
    assert!(
        views
            .iter()
            .any(|view| view.address.resource == message && view.address.action == "reveal-message"),
        "the message offers the view its link opens: {views:?}"
    );
}

/// Every message `page` keeps, by the id a path names it by, in the order its
/// parts hold them: a message at 300 or more within its part stands past the
/// page that part publishes.
fn held_messages(page: &tilo_agent_session::Replica) -> Vec<String> {
    let mut held = Vec::new();
    for index in 0..16 {
        let Some(part) = page.read_resource(&chat::parts::part(index), chat::messages::read_part)
        else {
            break;
        };
        held.extend(part.messages.iter().map(|(id, _)| id.as_str().to_owned()));
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

/// A message named by its id, as an agent's path names one and acts on it:
/// the chain a path starts with named at the app's own level, the message found
/// under it, its action looked up by address, edited and deleted, in a
/// replica opened afresh over what the workspace holds, as the device serving
/// the workspace opens one for a call. None of it builds the tree the chat publishes or lays its
/// view out, at twenty-five messages as at a thousand, and a message past the
/// page its part publishes is found and acted on the same way.
#[test]
fn a_message_named_by_its_id_is_acted_on_without_building_the_tree() {
    const LIST: usize = 100;
    for size in [25, 1000] {
        let mut page = founded();
        for start in (0..size).step_by(LIST) {
            post(&mut page, start, (start + LIST).min(size));
        }
        let held = held_messages(&page);
        assert_eq!(held.len(), size, "the chat keeps every message posted");
        let Some(DocumentMutation::Snapshot(chat)) = page.take_mutation().unwrap() else {
            panic!("a new chat is published whole");
        };
        let parts = page.take_document_mutations().unwrap();
        let mut targets = vec![held[100.min(size - 1)].clone()];
        if size > PAST_PAGE {
            targets.push(held[PAST_PAGE].clone());
        }
        // A path names the chain the app keeps at its own level
        // (`chat/Messages/<id>`) before the item: the app answers the name
        // from the chain's head part, never a page of the listing built from
        // every item the app shows.
        let named_chain = tilo_actions::remote::CatalogueQuery {
            parent_resource: None,
            resource_name: Some("Messages".to_owned()),
            max_results: 50,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        for target in targets {
            let mut fresh = crate::common::open(
                "so.tilo.chat",
                &MemberId::person("ada"),
                ReplicaActor::Owner,
                Seed::Existing {
                    record: page.document_id().clone(),
                    changes: vec![chat.clone()],
                },
            )
            .unwrap();
            for (id, part) in &parts {
                fresh.apply_document(id, part).unwrap();
            }
            let named = tilo_actions::remote::CatalogueQuery {
                parent_resource: Some(messages()),
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
                        .is_some_and(|action| action.address.action == "post")),
                    "the app's level names the chain a path starts with, at {size} messages: {chain:#?}"
                );
                let (_, rows) = fresh.survey_page(&named, (false, (0, 50)));
                let item = rows.iter().find_map(|row| row.action.as_ref()).map_or_else(
                    || panic!("{target} is found under the chain"),
                    |action| action.address.resource.clone(),
                );
                for (action, arguments, confirmed) in [
                    ("edit-message", serde_json::json!({"text": "Edited by id"}), false),
                    ("delete-message", serde_json::json!({}), true),
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
                "{target} of {size} messages was acted on by building the tree, laying it out or reading its whole part"
            );
            assert!(
                !held_messages(&fresh).contains(&target),
                "{target} of {size} messages is deleted"
            );
        }
    }
}

/// A chat listed again by a member whose access was read again since, unchanged, lists
/// without laying its view out or building its tree: the workspace reads a caller's access again
/// every half minute, and taking the same access in again laid the view out and built its whole
/// tree, which for a chat of thousands of messages cost the next listing most of its time.
#[test]
fn a_chat_listed_again_after_its_access_is_read_again_reuses_its_tree() {
    let mut made = founded();
    post(&mut made, 0, 20);
    let Some(DocumentMutation::Snapshot(chat)) = made.take_mutation().unwrap() else {
        panic!("a new chat is published whole");
    };
    let parts = made.take_document_mutations().unwrap();
    let access = ui::permissions::PermissionContext::member_of(
        crate::common::WORKSPACE,
        &MemberId::person("ada"),
        ui::permissions::Role::Owner,
    )
    .access()
    .clone()
    .as_acting(tilo_action_policy::Acting::handed_agent("Claude Code", "ada").in_session("s1"))
    .unwrap();
    let member = |unix_time| ReplicaActor::Member {
        access: access.clone(),
        sandbox: Vec::new(),
        unix_time,
        agent_name: None,
        device: None,
    };
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("ada"),
        member(1_900_000_000),
        Seed::Existing {
            record: made.document_id().clone(),
            changes: vec![chat],
        },
    )
    .unwrap();
    for (id, part) in &parts {
        page.apply_document(id, part).unwrap();
    }
    let listing = tilo_actions::remote::CatalogueQuery {
        parent_resource: Some(messages()),
        max_results: 2000,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let ((first, _), built_first) =
        ui::tilo_resource::profiling::observe(|| page.survey_page(&listing, (false, (0, 2000))));
    assert!(first >= 20, "every message is listed: {first} rows");
    assert_eq!(
        built(&built_first).0,
        0,
        "the first listing walks the chain's level, not the tree the view publishes"
    );

    // The workspace reads the member's access again a minute later: nothing of it moved.
    page.renew(member(1_900_000_060)).unwrap();
    let (again, observed) =
        ui::tilo_resource::profiling::observe(|| page.survey_page(&listing, (false, (0, 2000))).0);
    assert_eq!(again, first, "the listing lists the same messages");
    assert_eq!(
        built(&observed),
        (0, 0, 0),
        "the chat's view was laid out and its tree built again for access that did not move"
    );
}

/// A chat held for a member whose workspace changes around them: another person admitted
/// leaves what the member may do as it was, so their view keeps what it laid out; the member
/// demoted to a viewer, then removed, may do less each time, so their view is laid out again
/// under what they hold now and refuses what they lost.
#[test]
fn a_chats_view_is_laid_out_again_only_when_its_members_own_access_moves() {
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

    let mut made = founded();
    post(&mut made, 0, 20);
    let Some(DocumentMutation::Snapshot(chat)) = made.take_mutation().unwrap() else {
        panic!("a new chat is published whole");
    };
    let parts = made.take_document_mutations().unwrap();

    let member = |policy: &tilo_action_policy::WorkspacePolicy, unix_time| ReplicaActor::Member {
        access: tilo_action_policy::MemberAccess::person(policy.clone(), ADA)
            .as_acting(
                tilo_action_policy::Acting::handed_agent("Claude Code", ADA).in_session("s1"),
            )
            .unwrap(),
        sandbox: Vec::new(),
        unix_time,
        agent_name: None,
        device: None,
    };
    let mut page = crate::common::open(
        "so.tilo.chat",
        &MemberId::person(ADA),
        member(&policy, 1_900_000_000),
        Seed::Existing {
            record: made.document_id().clone(),
            changes: vec![chat],
        },
    )
    .unwrap();
    for (id, part) in &parts {
        page.apply_document(id, part).unwrap();
    }
    let listing = tilo_actions::remote::CatalogueQuery {
        parent_resource: Some(messages()),
        max_results: 2000,
        ..tilo_actions::remote::CatalogueQuery::default()
    };
    let (first, _) = page.survey_page(&listing, (false, (0, 2000)));
    assert!(first >= 20, "every message is listed: {first} rows");
    let post_one = |page: &mut tilo_agent_session::Replica, text: &str| {
        page.invoke(
            &ActionAddress::new(messages(), "post"),
            serde_json::json!({"channel": CHANNEL, "text": text})
                .as_object()
                .unwrap(),
            false,
        )
    };
    post_one(&mut page, "Posted as an editor").expect("an editor posts a message");
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
        "the listing lists what it did, messages and actions"
    );
    post_one(&mut page, "Posted after bea joined").expect("ada still posts messages");

    // Ada is made a viewer: the view is laid out under that, and posting is refused.
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
        "a viewer still lists the chat's messages: {as_viewer} rows"
    );
    assert!(
        post_one(&mut page, "Posted as a viewer").is_err(),
        "a viewer posts no message"
    );

    // Ada is removed: the view is laid out again, and ada reads nothing of the chat.
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
        "a removed member lists none of the chat's messages"
    );
    assert!(
        post_one(&mut page, "Posted after removal").is_err(),
        "a removed member posts no message"
    );
}
