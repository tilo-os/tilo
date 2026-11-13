#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

//! An agent's card: the resource Console publishes for each client, the card its face in
//! the workspace's presence opens, and what that face asks of it (Stop, Revoke) reaching
//! the agent door once the card has read the door.

use serde_json::json;
use tilo_console::{App, ConsoleEffect};
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::prelude::*;
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tilo_actions::remote::{
    InvocationState, InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
};

const AGENT: &str = "console.agent:Claude Code";

fn asked(app: &mut TestApp<App>) -> Vec<InvokeRequest> {
    app.frame();
    app.harness()
        .take_collaboration_requests()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } => Some(request),
            _ => None,
        })
        .collect()
}

#[track_caller]
fn find(requests: &[InvokeRequest], target: &RemoteActionTarget) -> InvokeRequest {
    requests
        .iter()
        .find(|request| request.target == *target)
        .cloned()
        .unwrap_or_else(|| panic!("the window asks for {target:?}"))
}

fn answer(app: &mut TestApp<App>, request: &InvokeRequest, value: serde_json::Value) {
    let invocation = request.invocation;
    app.harness()
        .handle_collaboration_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Item {
                        invocation,
                        sequence: 1,
                        value,
                    },
                )),
            },
        });
    app.frame();
}

/// Claude Code's session "Claude Code #2", working now through grant `g1`.
fn run() -> wire::AgentRun {
    wire::AgentRun {
        id: "mcp:r2".into(),
        agent: wire::Author {
            name: "Claude Code #2".into(),
            identity: "agent:claude".into(),
            relation: wire::Relation::Software,
            on_behalf_of: Some("Ada".into()),
            kind: ui::tilo_actions::ActorKind::Agent,
            ..wire::Author::default()
        },
        workspace: Some("Storefront".into()),
        instruction: "Fix the checkout total".into(),
        decision: None,
        started_ms: 1,
        last_ms: 2,
        range: wire::Range {
            from: "aa".into(),
            to: "ab".into(),
        },
        actions: Vec::new(),
        undo_blocked: None,
        now: None,
    }
}

fn the_door() -> door::Door {
    door::Door {
        endpoint: "https://mcp.example".into(),
        grants: vec![door::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            runs: vec![door::Run {
                name: "Claude Code #2".into(),
                started_ms: 1,
            }],
            ..door::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// Console with its reads answered: the run, and the door that let it in.
fn opened() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let runs = find(&requests, &wire::target(wire::RUNS));
    let read = find(&requests, &door::target(door::DOOR));
    answer(
        &mut app,
        &runs,
        serde_json::to_value(wire::Runs {
            total: 1,
            runs: vec![run()],
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    answer(&mut app, &read, serde_json::to_value(the_door()).unwrap());
    app
}

fn argument(request: &InvokeRequest) -> serde_json::Value {
    match &request.argument {
        InvokeArgument::Json(map) => serde_json::Value::Object(map.clone()),
        _ => serde_json::Value::Null,
    }
}

/// The resource `AGENT` names, among `resources` or their children.
fn find_in(resources: &[ui::tilo_actions::ResourceSpec]) -> Option<ui::tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == AGENT)
            .then(|| resource.clone())
            .or_else(|| find_in(&resource.children))
    })
}

#[test]
fn each_agent_is_published_with_its_card_and_its_commands() {
    let mut app = opened();
    let resources = app.harness().resources();
    let agent = find_in(&resources).unwrap_or_else(|| panic!("{AGENT} is published"));
    assert_eq!(agent.kind, "console.agent");
    let [view] = &agent.views[..] else {
        panic!("an agent opens in its card, not {:?}", agent.views);
    };
    assert!(view.primary);
    assert_eq!(view.open_action, ConsoleEffect::VIEW_AGENT.id);
    for action in [
        "stop-agent",
        "undo-agent-run",
        "take-over-agent",
        "revoke-agent",
    ] {
        assert!(
            agent.actions.iter().any(|spec| spec.id == action),
            "the card offers {action}"
        );
    }
}

#[test]
fn stopping_an_agent_from_its_face_opens_its_card_and_ends_its_run() {
    let mut app = opened();
    app.call_on(AGENT, "stop-agent", json!({}))
        .expect("stop the agent");
    let requests = asked(&mut app);
    let stop = find(&requests, &door::target(door::STOP));
    assert_eq!(
        argument(&stop),
        json!({"grant": "g1", "run": "Claude Code #2"}),
        "the working run ends, its grant kept"
    );
    assert!(
        app.harness().targets().contains_key("console-agent-head"),
        "the card shows what came of it"
    );
    assert!(
        !asked(&mut app)
            .iter()
            .any(|request| request.target == door::target(door::STOP)),
        "asked once"
    );
}

#[test]
fn revoking_an_agent_takes_back_its_grant() {
    let mut app = opened();
    app.call_on(AGENT, "revoke-agent", json!({}))
        .expect("revoke the agent");
    let requests = asked(&mut app);
    let revoke = find(&requests, &door::target(door::REVOKE));
    assert_eq!(argument(&revoke), json!({"grant": "g1"}));
    assert!(
        app.harness()
            .targets()
            .contains_key("console-agent-access-row")
    );
}

/// The window someone reads at the top of the console: what its bar names.
fn window_name(app: &mut TestApp<App>) -> Option<String> {
    app.frame();
    ui::window_toolbar::declared()
        .and_then(|bar| bar.location)
        .map(|location| location.label)
}

/// The card's window is named by what it shows: the agent's own name, where every place
/// names its place, and never the app's twice.
#[test]
fn the_cards_window_is_named_by_the_agent() {
    let mut app = opened();
    assert_eq!(window_name(&mut app).as_deref(), Some("Agents"));
    app.call_on(AGENT, "view-agent", json!({}))
        .expect("open the card");
    assert_eq!(window_name(&mut app).as_deref(), Some("Claude Code"));
}
