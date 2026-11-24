//! A direct conversation is read by its people alone: in a workspace of Ada, Bo and the agent
//! Scout, Ada's conversation with Scout is Ada's and Scout's. Bo is not shown it in the sidebar,
//! cannot open it, search finds nothing of it for him, `std.may_read` says he does not read it,
//! his History lists none of its changes to him, and an agent mentioned in it that is not one of
//! its people reads nothing of it. A channel is everyone's. Two devices opening the same
//! conversation apart make one, and a conversation an earlier Chat kept with one person alone
//! comes in with both of its people, or with that person alone when no person opened it.

use tilo_action_policy::{
    AgentMember, Answer, Grant, Grantee, KindSet, MemberAccess, PersonId, Role, TreePath,
    WorkspacePolicy,
};
use tilo_actions::ResourceSpec;
use tilo_resource_store::history::verbs::HistoryView;
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, Value};
use tilolang_tilo::publish::{ActionTypes, Planner, publish_resource, resource_id};
use tilolang_tilo::runtime::ids::{AppId, MemberId};

use crate::fixtures::{Replica, target};

const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const BEFORE: (&str, &str) = (
    include_str!("chat_before_direct_people/app.tl"),
    include_str!("chat_before_direct_people/app.ids"),
);
const APP: &str = "so.tilo.chat";
const WORKSPACE: &str = "workspace";

fn person(id: &str) -> Actor {
    Actor::from(MemberId::person(id))
}

fn agent(id: &str) -> Actor {
    Actor::from(MemberId::agent(id))
}

/// Ada's workspace: Bo edits it, and the agents Scout and Helper are members: Scout acts for
/// Ada, Helper for Bo, each reading no more than the person it acts for.
fn policy() -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("bo".to_owned(), Role::Editor);
    for (agent, person) in [("scout", "ada"), ("helper", "bo")] {
        policy.agents.insert(
            agent.to_owned(),
            AgentMember {
                for_person: person.to_owned(),
            },
        );
    }
    policy
}

/// `name`'s device holding `doc`, its runtime told the workspace's members and who of them
/// reads what, as the window's instance tells it.
fn device(name: &str, doc: crate::fixtures::Workspace, packages: &[(&str, &str)]) -> Replica {
    device_with(name, policy(), doc, packages)
}

/// As [`device`], in a workspace governed by `policy`: a test of who reads what names the
/// members and grants that decide it.
fn device_with(
    name: &str,
    policy: WorkspacePolicy,
    doc: crate::fixtures::Workspace,
    packages: &[(&str, &str)],
) -> Replica {
    let access = MemberAccess::person(policy, name);
    let (replica, _) = Replica::access(name, doc, packages, Some(access.clone()));
    let me = PersonId::from(name);
    replica.host.runtime().set_members(
        tilolang_tilo::members::members_of(&access, &me),
        Some(tilolang_tilo::members::reads_of(
            &access,
            &me,
            &replica.host,
        )),
    );
    replica
}

fn chat(name: &str, doc: crate::fixtures::Workspace) -> Replica {
    device(name, doc, &[CHAT])
}

/// The action `action` of Chat, invoked by the person of `r`.
fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    call_as(r, &person(&r.name), action, args)
}

/// The action `action` of Chat, invoked by `by`: whichever of the device's packages offers it,
/// as the earlier Chat and the one that replaces it name an action alike.
fn call_as(r: &Replica, by: &Actor, action: &str, args: Vec<Value>) -> Value {
    invoke_as(r, by, action, args).unwrap_or_else(|why| panic!("{action}: {why}"))
}

/// `action` of Chat invoked by `by`, answering what it returned or why it was not done.
fn invoke_as(r: &Replica, by: &Actor, action: &str, args: Vec<Value>) -> Result<Value, String> {
    let id = action_id(r, action);
    r.host
        .runtime()
        .invoke_action(by, (id.as_str(), args))
        .map_err(|error| error.to_string())
        .and_then(|done| match done {
            InvokeOutcome::Done(v) => Ok(v),
            other => Err(format!("{other:?}")),
        })
}

/// The id of `action` among the device's packages.
fn action_id(r: &Replica, action: &str) -> String {
    r.host
        .find_in_packages(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|a| a.name == action)
                .map(|a| a.id.clone())
        })
        .unwrap_or_else(|| panic!("no action {action}"))
}

/// Commit everything the runtime committed, the asks its mentions made included.
fn settle(r: &Replica) {
    while r.commit() > 0 {}
}

fn rich(text: &str) -> Value {
    Value::Rich(RichValue::from_text(text))
}

fn listed(r: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match r.host.runtime().read(at, field).unwrap() {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("a collection reads as a list: {other:?}"),
    }
}

/// `who` in the order a set keeps them: an assertion about who is in a conversation does not
/// depend on the order the set reads them in.
fn sorted(mut who: Vec<Actor>) -> Vec<Actor> {
    who.sort();
    who
}

fn people(r: &Replica, direct: &ResourceRef) -> Vec<Actor> {
    match r.host.runtime().read(direct, "people").unwrap() {
        Value::List(people) => people
            .into_iter()
            .map(|who| match who {
                Value::Actor(who) => who,
                other => panic!("a person: {other:?}"),
            })
            .collect(),
        other => panic!("its people are a list: {other:?}"),
    }
}

/// The conversations the sidebar of `r` places, by the item each place opens.
fn placed(r: &Replica) -> Vec<String> {
    let sidebar = r.view(APP);
    sidebar
        .tree()
        .of_kind("ui.NavItem")
        .into_iter()
        .filter_map(|place| match place.props.get("item") {
            Some(Value::Ref(item)) => Some(item.target.id_string()),
            _ => None,
        })
        .collect()
}

/// Every text search finds for the person of `r`.
fn found(r: &Replica) -> Vec<String> {
    tilolang_tilo::search::every_hit(&r.host)
        .into_iter()
        .flat_map(|hit| hit.texts)
        .collect()
}

/// Ada's workspace on her device and on Bo's: a channel everyone talks in, and Ada's direct
/// conversation with Scout, each with a message.
struct Workspace {
    ada: Replica,
    bo: Replica,
    general: Value,
    with_scout: Value,
}

fn workspace() -> Workspace {
    let ada = chat("ada", crate::fixtures::Workspace::new());
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let general = call(
        &ada,
        "new_channel",
        vec![all.clone(), Value::text("general")],
    );
    let with_scout = call(
        &ada,
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    settle(&ada);
    call(
        &ada,
        "send",
        vec![general.clone(), rich("Lunch at noon"), Value::none()],
    );
    call(
        &ada,
        "send",
        vec![
            with_scout.clone(),
            rich("The salaries sheet is ready"),
            Value::none(),
        ],
    );
    settle(&ada);
    let bo = chat("bo", ada.doc.fork());
    bo.commit();
    Workspace {
        ada,
        bo,
        general,
        with_scout,
    }
}

#[test]
fn a_direct_conversation_is_read_by_its_people_and_a_channel_by_everyone() {
    let Workspace {
        ada,
        bo,
        general,
        with_scout,
    } = workspace();
    let with_scout = target(&with_scout);
    let general = target(&general);
    assert_eq!(
        sorted(people(&ada, &with_scout)),
        sorted(vec![person("ada"), agent("scout")])
    );

    let message = listed(&ada, &with_scout, "messages")
        .pop()
        .expect("Ada's message");
    for r in [&ada, &bo] {
        let runtime = r.host.runtime();
        for at in [&with_scout, &message] {
            assert!(
                runtime.may_read(&person("ada"), at),
                "Ada reads her conversation"
            );
            assert!(
                runtime.may_read(&agent("scout"), at),
                "and Scout, who is in it"
            );
            assert!(
                !runtime.may_read(&person("bo"), at),
                "Bo, who is not in it, does not, though he edits the workspace"
            );
            assert!(
                !runtime.may_read(&agent("helper"), at),
                "nor an agent admitted to all of the workspace"
            );
        }
        for who in [person("ada"), person("bo"), agent("scout")] {
            assert!(
                runtime.may_read(&who, &general),
                "{who:?} reads the channel"
            );
        }
    }

    let shown = with_scout.id_string();
    assert!(placed(&ada).contains(&shown), "Ada's sidebar shows it");
    assert!(
        !placed(&bo).contains(&shown),
        "Bo's does not: {:?}",
        placed(&bo)
    );
    assert!(
        placed(&bo).contains(&general.id_string()),
        "the channel is his too"
    );
    assert!(
        bo.host.open_resource(&with_scout, None).is_err(),
        "Bo cannot open it"
    );
    // Ada opens it: the sidebar places it for her, search finds what is said in it and
    // `std.may_read` answers she reads it, all above. Its default view is not opened here: a
    // conversation `message_person` makes has no `person`, which that view reads.

    let ada_finds = found(&ada);
    assert!(
        ada_finds.iter().any(|t| t.contains("salaries")),
        "{ada_finds:?}"
    );
    let bo_finds = found(&bo);
    assert!(
        bo_finds.iter().any(|t| t.contains("Lunch at noon")),
        "Bo finds the channel's message: {bo_finds:?}"
    );
    assert!(
        !bo_finds.iter().any(|t| t.contains("salaries")),
        "and nothing of Ada's conversation with Scout: {bo_finds:?}"
    );
}

/// What Ada's window publishes to the device's search index names the readers of her
/// conversation with Scout on every entry of it: the index keeps all of Chat's entries in one
/// slot, which Bo passes as a member who reads Chat, so the entry itself says it is Ada's and
/// Scout's alone. The channel's message names nobody: whoever reads Chat finds it.
#[test]
fn the_entries_a_window_publishes_name_who_alone_reads_a_direct_conversation() {
    let Workspace { ada, .. } = workspace();
    let entries = tilolang_tilo::search::entries(&ada.host);
    let readers_of = |text: &str| {
        let entry = entries
            .iter()
            .find(|entry| entry.title.contains(text))
            .unwrap_or_else(|| panic!("an entry reads {text:?}: {entries:?}"));
        entry.readers.clone().map(|mut readers| {
            readers.sort();
            readers
        })
    };
    assert_eq!(
        readers_of("salaries"),
        Some(vec!["ada".to_owned(), "agent:scout".to_owned()]),
        "Ada's message to Scout is found by the two of them alone"
    );
    assert_eq!(
        readers_of("Lunch at noon"),
        None,
        "the channel's message by whoever reads Chat"
    );
    let found_by = |text: &str, reader: Option<&str>| {
        entries
            .iter()
            .filter(|entry| entry.title.contains(text))
            .all(|entry| entry.found_by(reader))
    };
    assert!(found_by("salaries", Some("ada")));
    assert!(found_by("salaries", Some("agent:scout")));
    assert!(!found_by("salaries", Some("bo")), "Bo finds nothing of it");
    assert!(
        !found_by("salaries", Some("agent:helper")),
        "nor an agent admitted to all of the workspace"
    );
    assert!(!found_by("salaries", None), "nor a caller who is no member");
    assert!(found_by("Lunch at noon", Some("bo")));
}

#[test]
fn bo_messaging_scout_opens_his_own_conversation_with_scout() {
    let Workspace { bo, with_scout, .. } = workspace();
    let all = Value::resource(&bo.host.root_of(APP).unwrap());
    let his = call(
        &bo,
        "message_person",
        vec![all.clone(), Value::Actor(agent("scout"))],
    );
    settle(&bo);
    assert_ne!(
        target(&his).id_string(),
        target(&with_scout).id_string(),
        "Bo's conversation with Scout is not Ada's"
    );
    assert_eq!(
        sorted(people(&bo, &target(&his))),
        sorted(vec![person("bo"), agent("scout")])
    );
    let again = call(
        &bo,
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    assert_eq!(
        target(&again).id_string(),
        target(&his).id_string(),
        "and pressing Scout again opens it again"
    );
    assert!(placed(&bo).contains(&target(&his).id_string()));
}

#[test]
fn an_agent_mentioned_in_a_conversation_it_is_not_in_reads_nothing_of_it() {
    let Workspace {
        ada, with_scout, ..
    } = workspace();
    let mut body = RichValue::from_text("Can ");
    body.push_reference("@Helper", &Reference::Member(MemberId::agent("helper")));
    body.push_text(" check the numbers?");
    call(
        &ada,
        "send",
        vec![with_scout.clone(), Value::Rich(body), Value::none()],
    );
    settle(&ada);
    // The mention starts Helper's turn, which reads the conversation as Helper: nothing of it.
    let message = listed(&ada, &target(&with_scout), "messages")
        .pop()
        .expect("Ada's message");
    let runtime = ada.host.runtime();
    assert!(
        !runtime.may_read(&agent("helper"), &message),
        "Helper, mentioned where it does not read, reads nothing of what was said"
    );
    assert!(
        runtime.may_read(&agent("scout"), &message),
        "Scout, who is in it, reads what Ada wrote"
    );
}

#[test]
fn two_devices_opening_the_same_conversation_apart_make_one() {
    // Ada presses Bo on her device and Bo presses Ada on his, neither online: one conversation
    // of the two of them once they meet.
    let ada = chat("ada", crate::fixtures::Workspace::new());
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let bo = chat("bo", ada.doc.fork());
    let all = Value::resource(&root);
    let on_ada = call(
        &ada,
        "message_person",
        vec![all.clone(), Value::Actor(person("bo"))],
    );
    let on_bo = call(
        &bo,
        "message_person",
        vec![all, Value::Actor(person("ada"))],
    );
    settle(&ada);
    settle(&bo);
    ada.sync_with(&bo);
    assert_eq!(target(&on_ada).id_string(), target(&on_bo).id_string());
    for r in [&ada, &bo] {
        assert_eq!(
            listed(r, &root, "channels").len(),
            1,
            "one conversation of Ada and Bo, not two"
        );
        assert!(placed(r).contains(&target(&on_ada).id_string()));
    }
}

#[test]
fn a_conversation_an_earlier_chat_kept_with_one_person_comes_in_with_both() {
    let before = device("ada", crate::fixtures::Workspace::new(), &[BEFORE]);
    let all = Value::resource(&before.host.root_of(APP).unwrap());
    before.commit();
    let with_scout = call(
        &before,
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    before.commit();
    call(
        &before,
        "send",
        vec![with_scout.clone(), rich("The salaries sheet is ready")],
    );
    before.commit();
    let with_scout = target(&with_scout);
    assert!(
        before.host.runtime().may_read(&person("bo"), &with_scout),
        "the earlier Chat let every member read it"
    );

    let now = chat("ada", before.doc.fork());
    assert!(now.host.migrate() >= 1, "its migration runs");
    settle(&now);
    assert_eq!(
        sorted(people(&now, &with_scout)),
        sorted(vec![person("ada"), agent("scout")]),
        "who opened it, and who it was with"
    );
    assert!(!now.host.runtime().may_read(&person("bo"), &with_scout));
    assert!(now.host.runtime().may_read(&agent("scout"), &with_scout));
    assert!(placed(&now).contains(&with_scout.id_string()));
    assert_eq!(now.host.migrate(), 0, "it runs once");
}

/// The published id of the resource `at`.
fn published(at: &ResourceRef) -> String {
    resource_id(APP, &at.id_string())
}

/// The versions of the resources `at`, as the app's History lists them to `who` on `r`: what the
/// store holds of each, oldest first, as that reader reads them.
fn history_of(r: &Replica, who: &str, at: &[&ResourceRef]) -> Vec<serde_json::Value> {
    let meta = r.host.package(APP).unwrap().meta;
    let planner: std::sync::Arc<dyn Planner> = std::sync::Arc::new(r.host.handle());
    let words = r.host.runtime().words();
    let types = ActionTypes::new(&meta, &planner, &*words);
    let trees: Vec<ResourceSpec> = at
        .iter()
        .map(|at| {
            let declared = meta
                .type_named(&at.ty)
                .unwrap_or_else(|| panic!("the type of {}", at.id_string()));
            publish_resource(&types, &declared.name, &at.id_string(), "")
        })
        .collect();
    let trees: Vec<&ResourceSpec> = trees.iter().collect();
    let listed = tilolang_service::history(
        &r.host,
        &trees,
        &HistoryView::History { last: Some(1000) },
        &person(who),
    )
    .unwrap();
    listed["versions"].as_array().unwrap().clone()
}

#[test]
fn bo_s_history_lists_nothing_of_ada_s_conversation_with_scout() {
    let Workspace {
        ada,
        bo,
        general,
        with_scout,
    } = workspace();
    let with_scout = target(&with_scout);
    let general = target(&general);
    let message = listed(&ada, &with_scout, "messages")
        .pop()
        .expect("Ada's message");
    let channel_message = listed(&ada, &general, "messages")
        .pop()
        .expect("the channel's message");
    let theirs = [published(&with_scout), published(&message)];
    let is_theirs = |version: &serde_json::Value| {
        version["resource"]
            .as_str()
            .is_some_and(|resource| theirs.iter().any(|id| id == resource))
    };
    let all = [&with_scout, &message, &general, &channel_message];

    let ada_sees = history_of(&bo, "ada", &all);
    let of_theirs = ada_sees.iter().filter(|version| is_theirs(version)).count();
    assert!(
        of_theirs > 0,
        "Ada's History lists the changes of her conversation with Scout: {ada_sees:?}"
    );
    let bo_sees = history_of(&bo, "bo", &all);
    assert!(
        bo_sees.iter().all(|version| !is_theirs(version)),
        "Bo's lists none of them: {bo_sees:?}"
    );
    assert_eq!(
        bo_sees.len(),
        ada_sees.len() - of_theirs,
        "and every other change"
    );
    // The channel is everyone's: its changes are listed to both.
    let channel = published(&general);
    assert!(
        ada_sees
            .iter()
            .any(|version| version["resource"] == serde_json::json!(channel)),
        "Ada's History lists the channel's changes: {ada_sees:?}"
    );
    assert!(
        bo_sees
            .iter()
            .any(|version| version["resource"] == serde_json::json!(channel)),
        "Bo's History lists the channel's changes: {bo_sees:?}"
    );
}

#[test]
fn a_conversation_an_earlier_chat_kept_that_no_person_opened_is_its_one_person_s_alone() {
    let before = device("ada", crate::fixtures::Workspace::new(), &[BEFORE]);
    let all = Value::resource(&before.host.root_of(APP).unwrap());
    before.commit();
    // The app made it by itself, as an automation or a migration does: no person opened it.
    let made = call_as(
        &before,
        &Actor::App(AppId::from(APP)),
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    before.commit();
    let with_scout = target(&made);

    let now = chat("ada", before.doc.fork());
    assert!(now.host.migrate() >= 1, "its migration runs");
    settle(&now);
    assert_eq!(
        people(&now, &with_scout),
        [agent("scout")],
        "the one person it was with, never everyone"
    );
    for who in [person("ada"), person("bo")] {
        assert!(!now.host.runtime().may_read(&who, &with_scout), "{who:?}");
    }
    assert!(now.host.runtime().may_read(&agent("scout"), &with_scout));
}

/// A reader field naming an agent of a person the workspace does not have reads none of it: the
/// store refuses to place the conversation rather than seal it to nobody.
#[test]
fn a_conversation_naming_an_agent_of_no_member_is_refused() {
    let mut policy = policy();
    policy.agents.insert(
        "scout".to_owned(),
        AgentMember {
            for_person: "zoe".to_owned(),
        },
    );
    let ada = device_with("ada", policy, crate::fixtures::Workspace::new(), &[CHAT]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let invoked = invoke_as(
        &ada,
        &person("ada"),
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    let refused = ada.host.store().take_refusal().or_else(|| invoked.err());
    assert!(
        refused
            .as_ref()
            .is_some_and(|why| why.contains("reads none of it")),
        "an agent acting for no member places nothing, got {refused:?}"
    );
}

/// A reader field naming an agent whose own grant is smaller than its person's reads none of it:
/// the conversation is refused rather than sealed to the agent's person.
#[test]
fn a_conversation_naming_an_agent_a_denied_grant_stops_is_refused() {
    let mut policy = policy();
    policy.grants.push(Grant::new(
        "scout-denied",
        Grantee::agent("scout"),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Deny,
        Grantee::person("ada"),
    ));
    let ada = device_with("ada", policy, crate::fixtures::Workspace::new(), &[CHAT]);
    let all = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let invoked = invoke_as(
        &ada,
        &person("ada"),
        "message_person",
        vec![all, Value::Actor(agent("scout"))],
    );
    let refused = ada.host.store().take_refusal().or_else(|| invoked.err());
    assert!(
        refused
            .as_ref()
            .is_some_and(|why| why.contains("reads none of it")),
        "an agent its grant does not reach places nothing, got {refused:?}"
    );
}
