//! The workspace's assistant in Chat: what it runs on and the sub-agents it started, read from
//! the workspace's record of its agents as the session publishes it. The record is the
//! workspace's, not an app's: Chat reads it through `tilo.workspace` and keeps no copy, and the
//! step that makes an assistant is the record's own `create-agent`.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{ResourceRef, Value, ViewInstance};

use crate::fixtures::{Replica, Sent, Workspace, nodes, target};

const CHAT_APP: &str = "so.tilo.chat";
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);

/// The workspace's record of its agents, as a view of Chat names it: the session publishes it
/// at this id, typed by the workspace's package.
fn roster() -> ResourceRef {
    ResourceRef::parse_id(
        "workspace:tilo.workspace.agent",
        "tilo.workspace#WorkspaceAgent",
    )
    .expect("a resource id")
}

/// One agent, as the record reads it: every field the record declares.
fn agent(id: &str, name: &str, created_at: i64) -> Json {
    json!({
        "id": id,
        "name": name,
        "acts_for": "ada",
        "model": "google/gemini-3-pro",
        "mcp_client": null,
        "spend_limit_micros": 10_000_000,
        "spent": { "day": "2026-11-23", "cost_micros": 3_120_000 },
        "state": "active",
        "created_at": created_at,
        "instruction": null,
        "sub": null,
        "sub_depth": null,
        "sub_at_once": null,
    })
}

/// A sub-agent `main` started, for `job`, working as `role`.
fn sub_agent(id: &str, name: &str, main: &str, job: &str, role: &str, created_at: i64) -> Json {
    json!({
        "id": id,
        "name": name,
        "acts_for": "ada",
        "model": null,
        "mcp_client": null,
        "spend_limit_micros": null,
        "spent": null,
        "state": "active",
        "created_at": created_at,
        "instruction": null,
        "sub": {
            "started_by": main,
            "job": job,
            "role": role,
            "kind": "",
            "depth": 1,
            "task": null,
            "stands": "working",
            "since": 0,
            "line": "",
            "notes": [],
            "produced": [],
            "coding": null,
        },
        "sub_depth": null,
        "sub_at_once": null,
    })
}

/// Ada's workspace with Chat, whose roster reads `agents`, at the view `name`.
fn opened(agents: Vec<Json>, name: &str) -> (Replica, ViewInstance) {
    opened_as(agents, name, None)
}

/// As [`opened`], in a workspace whose members the policy decides: the access a session hands a
/// window when the workspace has a roster of its own.
fn opened_as(
    agents: Vec<Json>,
    name: &str,
    access: Option<tilo_action_policy::MemberAccess>,
) -> (Replica, ViewInstance) {
    open_view(Some(agents), name, access)
}

/// As [`opened`], in a window the session has not served the workspace's record to yet: a view
/// opened before its reading arrives.
fn opened_without_roster(name: &str) -> (Replica, ViewInstance) {
    open_view(None, name, None)
}

/// The window at the view `name`, reading the workspace's record only where `agents` stands.
fn open_view(
    agents: Option<Vec<Json>>,
    name: &str,
    access: Option<tilo_action_policy::MemberAccess>,
) -> (Replica, ViewInstance) {
    let (ada, _) = Replica::access("ada", Workspace::new(), &[CHAT], access);
    // The workspace's package, as a runtime that runs Chat loads it to read the record it
    // publishes: the record is the workspace's, not any app's the window runs.
    let metas = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
    let workspace = metas
        .iter()
        .find(|meta| meta.app == "tilo.workspace")
        .expect("the workspace's package")
        .clone();
    ada.host
        .runtime()
        .load_foreign(workspace)
        .expect("the workspace is loaded");
    if let Some(agents) = agents {
        ada.host
            .runtime()
            .set_reading(&roster(), &json!({ "agents": agents }).to_string())
            .expect("the roster reads from its app");
    }
    let all = ada.host.root_of(CHAT_APP).expect("the conversations");
    let view = ada
        .host
        .package(CHAT_APP)
        .expect("Chat runs here")
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the view's declaration: {name}"))
        .id
        .clone();
    let open = ada
        .host
        .open_resource(&all, Some(&view))
        .unwrap_or_else(|why| panic!("{name} opens: {why}"));
    (ada, open)
}

/// What the view shows as its words: every text a node reads, a row's title and facts as a
/// text's value.
fn shown(view: &ViewInstance) -> Vec<String> {
    nodes(view.tree())
        .into_iter()
        .flat_map(|node| {
            ["value", "title", "secondary", "label", "message"]
                .into_iter()
                .filter_map(|prop| node.props.get(prop))
        })
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn supervision_shows_what_the_assistant_plans_with_and_spent() {
    let (_ada, open) = opened(
        vec![agent("assistant", "Ada's assistant", 1)],
        "supervision",
    );
    let said = shown(&open);
    for expected in [
        "Supervision",
        "Ada's assistant is on",
        "google/gemini-3-pro",
        "3.12 of 10 a day",
    ] {
        assert!(
            said.iter().any(|said| said.contains(expected)),
            "{expected}: {said:?}"
        );
    }
}

#[test]
fn supervision_offers_to_make_an_assistant_and_the_record_makes_it() {
    let (ada, open) = opened(Vec::new(), "supervision");
    let said = shown(&open);
    assert!(
        said.iter()
            .any(|said| said.contains("You have no assistant in this workspace")),
        "{said:?}"
    );
    let create = ada
        .host
        .package(CHAT_APP)
        .expect("Chat runs here")
        .meta
        .views
        .iter()
        .flat_map(|view| &view.anchors)
        .find(|anchor| anchor.name == "create_assistant")
        .expect("the button's anchor")
        .id
        .clone();
    let sent = Rc::new(RefCell::new(Vec::new()));
    ada.host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    open.press(&create)
        .expect("the button invokes the workspace's record");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app,
            action,
            name,
            args,
            target,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one invocation of the record's verb: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), action.as_str(), name.as_str()),
        (
            "tilo.workspace",
            "tilo.workspace#WorkspaceAgent/create-agent",
            "create-agent"
        )
    );
    assert_eq!(target.as_ref(), Some(&roster()));
    assert!(
        args.iter()
            .any(|(key, value)| key == "name" && *value == Value::text("Assistant")),
        "{args:?}"
    );
}

#[test]
fn the_sub_agents_the_assistant_started_stand_with_their_job_role_and_progress() {
    let (_ada, open) = opened(
        vec![
            agent("assistant", "Ada's assistant", 1),
            sub_agent(
                "usage-drop",
                "Scout",
                "assistant",
                "Find out why sign-ups dropped",
                "Coder",
                2,
            ),
            // Another agent's sub-agent is not the assistant's.
            sub_agent(
                "other",
                "Other",
                "someone-else",
                "Something else",
                "Researcher",
                3,
            ),
        ],
        "sub_agents",
    );
    let said = shown(&open);
    assert!(
        said.iter()
            .any(|said| said.contains("Sub-agents of Ada's assistant")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|said| said.contains("Find out why sign-ups dropped")),
        "{said:?}"
    );
    assert!(said.iter().any(|said| said.contains("Coder")), "{said:?}");
    assert!(said.iter().any(|said| said.contains("Working")), "{said:?}");
    assert!(
        !said.iter().any(|said| said.contains("Something else")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|said| said.contains("cannot read the main chat")),
        "{said:?}"
    );
}

#[test]
fn a_workspace_without_an_assistant_says_so_in_the_sub_agents_place() {
    let (_ada, open) = opened(Vec::new(), "sub_agents");
    let said = shown(&open);
    assert!(
        said.iter()
            .any(|said| said.contains("You have no assistant in this workspace")),
        "{said:?}"
    );
}

#[test]
fn starting_a_sub_agent_asks_the_assistant_in_the_conversation_it_answers_in() {
    use tilo_action_policy::{
        Answer, Grant, Grantee, KindSet, MemberAccess, TreePath, WorkspacePolicy,
    };

    let mut policy = WorkspacePolicy::new("workspace", "ada");
    // Ada's assistant, admitted by her: an agent is the pair of its id and its person.
    policy.grants.push(Grant::new(
        "agent-member:assistant",
        Grantee::agent("assistant", "ada"),
        TreePath::workspace("workspace"),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    let (ada, mut open) = opened_as(
        vec![agent("assistant", "Ada's assistant", 1)],
        "start_sub_agent",
        Some(MemberAccess::person(policy, "ada")),
    );
    open.poll_patches().expect("the members reach the view");
    let said = shown(&open);
    assert!(
        said.iter()
            .any(|said| said.contains("starts it in your conversation")),
        "{said:?}"
    );
    let entry = crate::tasks::key_of(&open, "ui.TextInput", &|node| {
        node.props.get("placeholder") == Some(&Value::text("What the sub-agent should do"))
    });
    open.fire_with(&entry, "submit", Some(&Value::text("Find the number")))
        .expect("the job is typed and entered");
    open.poll_patches().expect("the window runs the ask");
    let all = ada.host.root_of(CHAT_APP).expect("the conversations");
    let channels = ada.read(&all, "channels");
    let said: Vec<String> = match &channels {
        Value::List(items) => items
            .iter()
            .map(target)
            .flat_map(|d| match ada.read(&d, "messages") {
                Value::List(messages) => messages
                    .iter()
                    .map(target)
                    .filter_map(|m| match ada.read(&m, "body") {
                        Value::Rich(rich) => Some(rich.written_text()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect(),
        other => panic!("the conversations' channels: {other:?}"),
    };
    // What the app does is ask: one message, the request an assistant's turn reads. Whether it
    // answers or starts the sub-agent is that turn's decision, made in the conversation where
    // its work stands, so nothing is decided before the message is sent.
    assert_eq!(
        said,
        ["Start a sub-agent for this job: Find the number"],
        "one request in the assistant's conversation: {channels:?}"
    );
}

#[test]
fn a_sub_agents_row_opens_its_own_page() {
    let (_ada, mut open) = opened(
        vec![
            agent("assistant", "Ada's assistant", 1),
            sub_agent(
                "usage-drop",
                "Scout",
                "assistant",
                "Find out why sign-ups dropped",
                "Coder",
                2,
            ),
        ],
        "sub_agents",
    );
    let closed = shown(&open);
    assert!(
        !closed
            .iter()
            .any(|said| said.contains("It cannot read the main chat")),
        "the page is closed until its row is chosen: {closed:?}"
    );
    let card = crate::tasks::key_of(&open, "ui.Card", &|node| {
        node.props.get("title") == Some(&Value::text("Find out why sign-ups dropped"))
    });
    open.fire(&card, "activate")
        .expect("the row opens the sub-agent");
    open.poll_patches().expect("the page runs");
    let said = shown(&open);
    for expected in [
        "Find out why sign-ups dropped",
        "Coder",
        "Scout",
        "It cannot read the main chat",
    ] {
        assert!(
            said.iter().any(|said| said.contains(expected)),
            "{expected}: {said:?}"
        );
    }
}

#[test]
fn the_sidebar_shows_the_person_looking_at_its_foot() {
    let (ada, mut open) = opened(Vec::new(), "sidebar");
    let all = ada.host.root_of(CHAT_APP).expect("the conversations");
    crate::chat::call(
        &ada,
        "new_channel",
        vec![Value::resource(&all), Value::text("design")],
    );
    open.poll_patches()
        .expect("the channel reaches the sidebar");
    let person = nodes(open.tree())
        .into_iter()
        .find(|node| node.props.contains_key("person"));
    assert!(
        person.is_some(),
        "the person looking stands as a place of their own: {:?}",
        shown(&open)
    );
}

#[test]
fn a_window_opened_before_the_agents_arrive_shows_them_when_they_do() {
    let (ada, mut open) = opened(Vec::new(), "supervision");
    assert!(
        shown(&open)
            .iter()
            .any(|said| said.contains("You have no assistant")),
        "no record yet is no assistant: {:?}",
        shown(&open)
    );
    ada.host
        .runtime()
        .set_reading(
            &roster(),
            &json!({ "agents": [agent("assistant", "Ada's assistant", 1)] }).to_string(),
        )
        .expect("the record arrives");
    open.poll_patches()
        .expect("the record's arrival reaches the view");
    assert!(
        shown(&open)
            .iter()
            .any(|said| said.contains("Ada's assistant is on")),
        "the window shows the assistant once its record is here: {:?}",
        shown(&open)
    );
}

#[test]
fn a_window_the_record_was_never_served_opens_without_claiming_none() {
    let (_ada, open) = opened_without_roster("supervision");
    let said = shown(&open);
    assert!(
        !said
            .iter()
            .any(|said| said.contains("You have no assistant")),
        "a record that is not here yet is not a workspace without an assistant: {said:?}"
    );
}
