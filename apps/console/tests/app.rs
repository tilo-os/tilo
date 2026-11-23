#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_console::App;
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::prelude::*;
use ui::tilo_actions::remote::device_tickets;
use ui::tilo_actions::remote::mcp_server;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tilo_actions::remote::{
    InvocationState, InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
};
use ui::tilo_actions::{InvocationId, InvocationUpdate};

fn english() {
    ui::i18n::I18nManager::init("en-US");
}

/// Whether the window says `text`, as a name, value or description a screen reader reads.
fn says(app: &mut TestApp<App>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    })
}

fn shows(app: &mut TestApp<App>, target: &str) -> bool {
    app.harness().targets().contains_key(target)
}

/// Press the way back the window's bar carries, as a person does.
fn back(app: &mut TestApp<App>) {
    app.harness()
        .press_window_part(ui::window_toolbar::ToolbarEvent::Action {
            id: ui::widget::WINDOW_BACK.to_owned(),
        });
}

#[test]
fn every_scenario_shows_its_place_built_from_components() {
    english();
    let cases: [(&str, &[&str]); 7] = [
        (
            "agents",
            &[
                "console-filter-agent",
                "console-filter-workspace",
                "console-session-fix-signup",
                "console-session-budget",
            ],
        ),
        (
            "session",
            &[
                "console-session-stop",
                "console-session-take-over",
                "console-session-undo",
                "console-action-0",
                "console-action-1-view",
                "console-action-1-undo",
                "console-action-3",
            ],
        ),
        (
            "command",
            &[
                "console-action-0-view",
                "console-action-0-view-open",
                "console-action-0-output",
                "console-action-4",
            ],
        ),
        ("command-output", &["console-action-0-output"]),
        (
            "workspaces",
            &["console-workspace-0-open", "console-workspace-1-open"],
        ),
        (
            "connect",
            &[
                "console-connect-commands",
                "console-grant-0-revoke",
                "console-grant-1-revoke",
            ],
        ),
        ("usage", &["console-usage-empty"]),
    ];
    for (scenario, targets) in cases {
        let mut app = TestApp::<App>::capture(Some(scenario)).expect("capture");
        for target in targets {
            assert!(shows(&mut app, target), "{scenario} shows {target}");
        }
    }
}

#[test]
fn the_agent_picker_offers_each_client_over_all_of_its_runs() {
    english();
    let mut app = TestApp::<App>::capture(Some("agents")).expect("capture");
    app.harness().click_named("console-filter-agent").unwrap();
    app.frame();
    let labels: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    let at = |label: &str| {
        labels
            .iter()
            .position(|seen| seen == label)
            .unwrap_or_else(|| panic!("{label} stands among {labels:?}"))
    };
    let (claude, codex) = (at("Every Claude Code session"), at("Every Codex session"));
    assert!(
        claude < at("Claude Code #3") && at("Claude Code #1") < codex,
        "each client's own runs stand under it, before the next client: {labels:?}"
    );
    assert!(claude < codex, "the clients keep one group each");
}

#[test]
fn picking_a_client_shows_every_run_of_it() {
    english();
    let mut app = TestApp::<App>::capture(Some("agents")).expect("capture");
    assert!(shows(&mut app, "console-session-release-notes"));
    app.harness()
        .perform_named(
            "console-filter-agent",
            &ui::a11y::Action::SetValue("Claude Code".into()),
        )
        .unwrap();
    app.frame();
    assert!(shows(&mut app, "console-session-fix-signup"));
    assert!(shows(&mut app, "console-session-budget"));
    assert!(
        !shows(&mut app, "console-session-release-notes"),
        "Codex's run is another client's"
    );
}

#[test]
fn the_sessions_back_shows_every_run_again() {
    english();
    let mut app = TestApp::<App>::capture(Some("session")).expect("capture");
    assert!(
        shows(&mut app, "console-session-undo"),
        "the run's own page"
    );
    back(&mut app);
    assert!(shows(&mut app, "console-session-fix-signup"), "the runs");
    assert!(!shows(&mut app, "console-session-undo"));
}

#[test]
fn a_program_or_a_page_offers_no_undo_and_a_stopped_agent_no_stop() {
    english();
    let mut app = TestApp::<App>::capture(Some("command")).expect("capture");
    assert!(
        !shows(&mut app, "console-action-0-undo"),
        "a program that ran changed no resource to take back"
    );
    assert!(!shows(&mut app, "console-action-3-undo"));
    let mut app = TestApp::<App>::capture(Some("agents")).expect("capture");
    app.harness()
        .click_named("console-session-release-notes")
        .unwrap();
    app.frame();
    assert!(shows(&mut app, "console-session-undo"));
    assert!(!shows(&mut app, "console-session-stop"));
}

/// The reads and writes the window asked for since last asked.
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

/// The request to `target` among `requests`; a missing one fails where the test names it.
#[track_caller]
fn find(requests: &[InvokeRequest], target: &RemoteActionTarget) -> InvokeRequest {
    requests
        .iter()
        .find(|request| request.target == *target)
        .cloned()
        .unwrap_or_else(|| panic!("the window asks for {target:?}"))
}

#[track_caller]
fn argument<T: serde::de::DeserializeOwned>(request: &InvokeRequest) -> T {
    let InvokeArgument::Json(map) = &request.argument else {
        panic!("the request carries its argument");
    };
    serde_json::from_value(serde_json::Value::Object(map.clone())).unwrap()
}

fn answer(app: &mut TestApp<App>, invocation: InvocationId, value: serde_json::Value) {
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

fn agent() -> wire::Author {
    wire::Author {
        name: "Claude Code #2".into(),
        identity: "agent:claude".into(),
        relation: wire::Relation::Software,
        on_behalf_of: Some("Ada".into()),
        ..wire::Author::default()
    }
}

fn run() -> wire::AgentSession {
    let target = wire::Target {
        app_id: Some("so.tilo.notes".into()),
        id: "plan".into(),
        label: "Plan".into(),
        ..wire::Target::default()
    };
    wire::AgentSession {
        id: "r1".into(),
        agent: agent(),
        workspace: Some("Launch".into()),
        instruction: "Tidy the plan".into(),
        decision: None,
        started_ms: 1,
        last_ms: 2,
        range: wire::Range {
            from: "aa".into(),
            to: "ab".into(),
        },
        actions: vec![wire::SessionAction {
            range: wire::Range::one("ab"),
            changes: 1,
            at_ms: 2,
            summary: "Rewrote the intro".into(),
            view: wire::ActionView::Record { target },
            app_id: "so.tilo.notes".into(),
            record: "record:plan".into(),
            undo: Some(wire::Offer {
                action: wire::DetailAction::UndoEntry,
                label: "Undo".into(),
            }),
        }],
        undo_blocked: None,
        now: Some(wire::Now::Running {
            command: "cargo test -p signup".into(),
            since_ms: 2,
        }),
    }
}

/// The server, with the run History names "Claude Code #2" open through it or not.
fn the_server(open: bool) -> mcp_server::Server {
    mcp_server::Server {
        endpoint: "https://mcp.example".into(),
        grants: vec![mcp_server::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            sessions: if open {
                vec![mcp_server::AgentSession {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }]
            } else {
                Vec::new()
            },
            ..mcp_server::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// Open the console on a session that answers with `run` and an MCP server where that run is
/// `open` or not.
fn opened(open: bool) -> TestApp<App> {
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let runs = find(&requests, &wire::target(wire::SESSIONS));
    let query: wire::SessionsQuery = argument(&runs);
    assert_eq!(
        query.scope,
        wire::Scope::AllWorkspaces,
        "the console reads every workspace's runs"
    );
    let server_read = find(&requests, &mcp_server::target(mcp_server::SERVER));
    answer(
        &mut app,
        runs.invocation,
        serde_json::to_value(wire::Sessions {
            sessions: vec![run()],
            total: 1,
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    answer(
        &mut app,
        server_read.invocation,
        serde_json::to_value(the_server(open)).unwrap(),
    );
    app
}

#[test]
fn stopping_a_working_run_asks_the_server_to_end_that_run_alone() {
    english();
    let mut app = opened(true);
    app.harness().click_named("console-session-r1").unwrap();
    asked(&mut app);
    app.harness().click_named("console-session-stop").unwrap();
    let requests = asked(&mut app);
    let stop: mcp_server::AgentSessionRef =
        argument(&find(&requests, &mcp_server::target(mcp_server::STOP)));
    assert_eq!(
        stop,
        mcp_server::AgentSessionRef {
            grant: "g1".into(),
            session: "Claude Code #2".into(),
        }
    );
}

#[test]
fn undoing_a_run_or_one_action_asks_the_history_by_what_it_changed() {
    english();
    let mut app = opened(false);
    app.harness().click_named("console-session-r1").unwrap();
    asked(&mut app);
    assert!(
        !shows(&mut app, "console-session-stop"),
        "a run whose session ended has nothing to stop"
    );

    app.harness().click_named("console-session-undo").unwrap();
    let requests = asked(&mut app);
    let range: wire::Range = argument(&find(&requests, &wire::target(wire::UNDO_SESSION)));
    assert_eq!(range, run().range);

    app.harness().click_named("console-action-0-undo").unwrap();
    let requests = asked(&mut app);
    let one: wire::ResourceQuery = argument(&find(&requests, &wire::target(wire::UNDO_RECORD)));
    assert_eq!(one.range, run().range, "the run it is part of");
    assert_eq!(
        (one.app_id.as_str(), one.id.as_str()),
        ("so.tilo.notes", "record:plan"),
        "the resource the action changed, not the resource its view shows"
    );
}

#[test]
fn opening_an_action_shows_what_it_changed_and_reads_the_comparison() {
    english();
    let mut app = opened(false);
    app.harness().click_named("console-session-r1").unwrap();
    asked(&mut app);
    assert!(!shows(&mut app, "console-action-0-view"));
    app.harness().click_named("console-action-0").unwrap();
    let requests = asked(&mut app);
    assert!(shows(&mut app, "console-action-0-view"));
    let detail: wire::DetailQuery = argument(&find(&requests, &wire::target(wire::DETAIL)));
    assert_eq!(detail.range, wire::Range::one("ab"));
    assert!(
        detail.compare,
        "the view compares either side of the action"
    );
}

/// The console open at Connect, its server listing one agent whose key does or does not
/// operate web pages in Browser.
fn connect(browser_pages: bool) -> TestApp<App> {
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let runs = find(&requests, &wire::target(wire::SESSIONS));
    let server_read = find(&requests, &mcp_server::target(mcp_server::SERVER));
    answer(
        &mut app,
        runs.invocation,
        serde_json::to_value(wire::Sessions {
            sessions: vec![run()],
            total: 1,
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    let mut server = the_server(false);
    server.grants[0].browser_pages = browser_pages;
    answer(
        &mut app,
        server_read.invocation,
        serde_json::to_value(server).unwrap(),
    );
    let targets: Vec<String> = app.harness().targets().into_keys().collect();
    let connect = targets
        .iter()
        .find(|target| target.contains("NavList[0]/row[") && target.ends_with("connect]"))
        .cloned()
        .unwrap_or_else(|| panic!("the sidebar offers Connect among {targets:?}"));
    app.harness().click_named(&connect).unwrap();
    app
}

/// A key that operates web pages in Browser says so in the connected agents list, beside
/// what it reaches; a key asked about every page action says nothing of it, and revoking
/// the agent whose key does takes that key back at the server.
#[test]
fn the_connected_agents_list_says_when_a_key_operates_web_pages() {
    english();
    let mut operating = connect(true);
    assert!(
        says(&mut operating, "Browser pages"),
        "the agent's key operates web pages in Browser, and its row says so"
    );
    operating
        .harness()
        .click_named("console-grant-0-revoke")
        .unwrap();
    let requests = asked(&mut operating);
    let revoke: device_tickets::Revoke = argument(&find(
        &requests,
        &device_tickets::target(device_tickets::REVOKE),
    ));
    assert_eq!(
        revoke.device, "g1",
        "revoking that agent ends the key that operates web pages"
    );
    let mut asked = connect(false);
    assert!(
        !says(&mut asked, "Browser pages"),
        "a key asked about every page action says nothing of it"
    );
}

/// A server that keeps no MCP grants for this person (someone else's, which their page is in)
/// answers no read of them: Connect offers the setup that signs their agent in as them, the one
/// their own card shows, instead of a failure.
#[test]
fn connect_on_a_server_that_keeps_no_grants_for_the_person_offers_their_setup() {
    use ui::tilo_actions::remote::{
        ActionCallError, InvocationReceipt, ReceiptStatus, session_actions,
    };
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let server_read = find(&requests, &mcp_server::target(mcp_server::SERVER));
    app.harness()
        .handle_collaboration_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: server_read.invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::UnknownAction,
                },
            }),
        });
    app.frame();
    let targets: Vec<String> = app.harness().targets().into_keys().collect();
    let connect = targets
        .iter()
        .find(|target| target.contains("NavList[0]/row[") && target.ends_with("connect]"))
        .cloned()
        .unwrap_or_else(|| panic!("the sidebar offers Connect among {targets:?}"));
    app.harness().click_named(&connect).unwrap();
    app.frame();
    assert!(
        !says(&mut app, "no such action is offered"),
        "the refusal is no failure the person reads"
    );
    app.harness().click_named("console-connect-setup").unwrap();
    let requests = asked(&mut app);
    find(
        &requests,
        &session_actions::target(session_actions::SHOW_CONNECT),
    );
}

#[test]
fn revoking_a_grant_names_it_to_the_server() {
    english();
    let mut app = opened(true);
    let targets: Vec<String> = app.harness().targets().into_keys().collect();
    let connect = targets
        .iter()
        .find(|target| target.contains("NavList[0]/row[") && target.ends_with("connect]"))
        .cloned()
        .unwrap_or_else(|| panic!("the sidebar offers Connect among {targets:?}"));
    app.harness().click_named(&connect).unwrap();
    let requests = asked(&mut app);
    if let Some(read) = requests
        .iter()
        .find(|request| request.target == mcp_server::target(mcp_server::SERVER))
    {
        answer(
            &mut app,
            read.invocation,
            serde_json::to_value(the_server(true)).unwrap(),
        );
    }
    app.harness().click_named("console-grant-0-revoke").unwrap();
    let requests = asked(&mut app);
    let revoke: device_tickets::Revoke = argument(&find(
        &requests,
        &device_tickets::target(device_tickets::REVOKE),
    ));
    assert_eq!(revoke.device, "g1");
}

/// A run that is working says what it is doing now on its row, from the history's live
/// read; once its session ended, the row names the agent again rather than a command that
/// is no longer running.
#[test]
fn a_working_run_says_what_it_is_doing_now() {
    english();
    let mut app = opened(true);
    assert!(
        says(&mut app, "Running cargo test -p signup…"),
        "the working run's row says the command it is running"
    );
    let mut app = opened(false);
    assert!(!says(&mut app, "Running cargo test -p signup…"));
}

/// German is one of the languages the app ships, and it reads in German: the copy comes
/// from `locales/de-DE.ftl`, never from the English source beside it, so a device set to
/// German reads a German window.
#[test]
fn a_german_window_reads_the_catalogue() {
    let catalogue = &tilo_console::locales::CATALOG;
    assert!(
        catalogue.languages().contains(&"de-DE"),
        "the app ships German among {:?}",
        catalogue.languages()
    );
    catalogue.set_locale("de-DE");
    assert_eq!(catalogue.t("console-title"), "Konsole");
    catalogue.set_locale("en-US");
    assert_eq!(catalogue.t("console-title"), "Console");
}
