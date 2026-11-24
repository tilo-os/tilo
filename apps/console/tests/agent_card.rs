//! An agent's card: the resource Console publishes for each client, the card its face in
//! the workspace's presence opens, and what that face asks of it (Stop, Revoke) reaching
//! the MCP server once the card has read it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use serde_json::json;
use tilo_app_test_host::TestApp;
use tilo_console::{App, ConsoleEffect};
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::device_tickets;
use ui::tilo_actions::remote::mcp_server;
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
                correlation: None,
            },
        });
    app.frame();
}

/// Claude Code's session "Claude Code #2", working now through grant `g1`.
fn run() -> wire::AgentSession {
    wire::AgentSession {
        id: "mcp:r2".into(),
        agent: wire::Author {
            name: "Claude Code #2".into(),
            identity: "agent:claude".into(),
            relation: wire::Relation::Software,
            on_behalf_of: Some("Ada".into()),
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

fn the_server() -> mcp_server::Server {
    mcp_server::Server {
        endpoint: "https://mcp.example".into(),
        grants: vec![mcp_server::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            sessions: vec![mcp_server::AgentSession {
                name: "Claude Code #2".into(),
                started_ms: 1,
            }],
            ..mcp_server::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// Console with its reads answered: the run, and the MCP server that let it in.
fn opened() -> TestApp<App> {
    opened_on(the_server())
}

/// [`opened`] on `server`, the MCP server as the person's card reads it.
fn opened_on(server: mcp_server::Server) -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let runs = find(&requests, &wire::target(wire::SESSIONS));
    let read = find(&requests, &mcp_server::target(mcp_server::SERVER));
    answer(
        &mut app,
        &runs,
        serde_json::to_value(wire::Sessions {
            total: 1,
            sessions: vec![run()],
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    answer(&mut app, &read, serde_json::to_value(server).unwrap());
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
        "undo-agent-session",
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
fn stopping_an_agent_from_its_face_opens_its_card_and_ends_its_session() {
    let mut app = opened();
    app.call_on(AGENT, "stop-agent", json!({}))
        .expect("stop the agent");
    let requests = asked(&mut app);
    let stop = find(&requests, &mcp_server::target(mcp_server::STOP));
    assert_eq!(
        argument(&stop),
        json!({"grant": "g1", "session": "Claude Code #2"}),
        "the working session ends, its grant kept"
    );
    assert!(
        app.harness().targets().contains_key("console-agent-head"),
        "the card shows what came of it"
    );
    assert!(
        !asked(&mut app)
            .iter()
            .any(|request| request.target == mcp_server::target(mcp_server::STOP)),
        "asked once"
    );
}

#[test]
fn a_stopped_agent_offers_the_way_on() {
    let mut server = the_server();
    server.grants[0].stopped = true;
    let mut app = opened_on(server);
    app.harness()
        .click_named("console-agent-go-on")
        .expect("the way on is offered");
    let requests = asked(&mut app);
    let go_on = find(&requests, &mcp_server::target(mcp_server::GO_ON));
    assert_eq!(
        argument(&go_on),
        json!({"grant": "g1", "session": "Claude Code"}),
        "its calls are answered again"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.target == mcp_server::target(mcp_server::STOP)),
        "a stopped agent is not stopped again"
    );
}

#[test]
fn revoking_an_agent_takes_back_its_grant() {
    let mut app = opened();
    app.call_on(AGENT, "revoke-agent", json!({}))
        .expect("revoke the agent");
    let requests = asked(&mut app);
    let revoke = find(&requests, &device_tickets::target(device_tickets::REVOKE));
    assert_eq!(
        argument(&revoke),
        json!({"device": "g1", "reason": "retired"}),
        "an agent's key is taken back as every key of the person's is"
    );
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
