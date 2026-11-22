//! A direct conversation is read by its people alone: in a workspace of Ada, Bo and the agent
//! Scout, Ada's conversation with Scout is Ada's and Scout's. Bo is not shown it in the sidebar,
//! cannot open it, search finds nothing of it for him, `std.may_read` says he does not read it,
//! and an agent mentioned in it that is not one of its people is asked with nothing of it. A
//! channel is everyone's. Two devices opening the same conversation apart make one, and a
//! conversation an earlier Chat kept with one person alone comes in with both of its people.

use automerge::AutoCommit;
use tilo_action_policy::{
    Answer, Grant, Grantee, KindSet, MemberAccess, PersonId, Role, TreePath, WorkspacePolicy,
};
use tilolang_runtime::rich::{Reference, RichValue};
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, Value};
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, target};

const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const BEFORE: (&str, &str) = (
    include_str!("chat_before_direct_people/app.tl"),
    include_str!("chat_before_direct_people/app.ids"),
);
const APP: &str = "so.tilo.lang.chat";
const AGENT_APP: &str = "so.tilo.lang.agent";
const WORKSPACE: &str = "workspace";

fn person(id: &str) -> Actor {
    Actor::from(MemberId::person(id))
}

fn agent(id: &str) -> Actor {
    Actor::from(MemberId::agent(id))
}

/// Ada's workspace: Bo edits it, and the agents Scout and Helper are admitted to read and write
/// all of it.
fn policy() -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("bo".to_owned(), Role::Editor);
    for (grant, name) in [
        ("agent-member:scout", "scout"),
        ("agent-member:helper", "helper"),
    ] {
        policy.grants.push(Grant::new(
            grant,
            Grantee::agent(name),
            TreePath::workspace(WORKSPACE),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person("ada"),
        ));
    }
    policy
}

/// `name`'s device holding `doc`, its runtime told the workspace's members and who of them
/// reads what, as the window's instance tells it.
fn device(name: &str, doc: AutoCommit, packages: &[(&str, &str)]) -> Replica {
    let access = MemberAccess::person(policy(), name);
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

fn chat(name: &str, doc: AutoCommit) -> Replica {
    device(name, doc, &[crate::fixtures::AGENT, CHAT])
}

/// The action `action` of Chat, invoked by the person of `r`.
fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&person(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// Commit everything the runtime committed, the asks its mentions made included.
fn settle(r: &mut Replica) {
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
    let _ = r.host.wait_index();
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
    let mut ada = chat("ada", AutoCommit::new());
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
    settle(&mut ada);
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
    settle(&mut ada);
    let mut bo = chat("bo", ada.doc.fork());
    bo.host.sync_from(&bo.doc);
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
    assert_eq!(people(&ada, &with_scout), [person("ada"), agent("scout")]);

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
    assert!(ada.host.open_resource(&with_scout, None).is_ok());

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

#[test]
fn bo_messaging_scout_opens_his_own_conversation_with_scout() {
    let Workspace {
        mut bo, with_scout, ..
    } = workspace();
    let all = Value::resource(&bo.host.root_of(APP).unwrap());
    let his = call(
        &bo,
        "message_person",
        vec![all.clone(), Value::Actor(agent("scout"))],
    );
    settle(&mut bo);
    assert_ne!(
        target(&his).id_string(),
        target(&with_scout).id_string(),
        "Bo's conversation with Scout is not Ada's"
    );
    assert_eq!(people(&bo, &target(&his)), [person("bo"), agent("scout")]);
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

/// The asks of the agent `agent`, each with whether something was said in it.
fn asked(r: &Replica, agent: &Actor) -> Vec<bool> {
    let root = r.host.root_of(AGENT_APP).unwrap();
    listed(r, &root, "asks")
        .iter()
        .filter(|ask| r.host.runtime().read(ask, "agent").unwrap() == Value::Actor(agent.clone()))
        .map(|ask| {
            matches!(
                r.host.runtime().read(ask, "said").unwrap(),
                Value::Opt(Some(_))
            )
        })
        .collect()
}

#[test]
fn an_agent_mentioned_in_a_conversation_it_is_not_in_is_asked_with_nothing_of_it() {
    let Workspace {
        mut ada,
        with_scout,
        ..
    } = workspace();
    let mut body = RichValue::from_text("Can ");
    body.push_reference("@Helper", &Reference::Member(MemberId::agent("helper")));
    body.push_text(" check the numbers?");
    call(
        &ada,
        "send",
        vec![with_scout, Value::Rich(body), Value::none()],
    );
    settle(&mut ada);
    assert_eq!(
        asked(&ada, &agent("helper")),
        [false],
        "Helper is asked, with nothing of what was said where it does not read"
    );
    assert!(
        asked(&ada, &agent("scout")).iter().all(|said| *said),
        "Scout, who is in it, is told what Ada wrote"
    );
}

#[test]
fn two_devices_opening_the_same_conversation_apart_make_one() {
    // Ada presses Bo on her device and Bo presses Ada on his, neither online: one conversation
    // of the two of them once they meet.
    let mut ada = chat("ada", AutoCommit::new());
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let mut bo = chat("bo", ada.doc.fork());
    bo.host.sync_from(&bo.doc);
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
    settle(&mut ada);
    settle(&mut bo);
    ada.sync_with(&mut bo);
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
    let mut before = device("ada", AutoCommit::new(), &[BEFORE]);
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

    let mut now = chat("ada", before.doc.fork());
    now.host.sync_from(&now.doc);
    assert!(now.host.migrate() >= 1, "its migration runs");
    settle(&mut now);
    assert_eq!(
        people(&now, &with_scout),
        [person("ada"), agent("scout")],
        "who opened it, and who it was with"
    );
    assert!(!now.host.runtime().may_read(&person("bo"), &with_scout));
    assert!(now.host.runtime().may_read(&agent("scout"), &with_scout));
    assert!(placed(&now).contains(&with_scout.id_string()));
    assert_eq!(now.host.migrate(), 0, "it runs once");
}
