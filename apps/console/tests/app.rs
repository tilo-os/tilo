#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_console::App;
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tilo_actions::remote::{
    InvocationState, InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
};
use ui::tilo_actions::{InvocationId, InvocationUpdate};

fn english() {
    ui::i18n::I18nManager::init("en-US");
}

fn shows(app: &mut TestApp<App>, target: &str) -> bool {
    app.harness().targets().contains_key(target)
}

#[test]
fn every_scenario_shows_its_place_built_from_components() {
    english();
    let cases: [(&str, &[&str]); 6] = [
        (
            "agents",
            &[
                "console-filter-agent",
                "console-filter-workspace",
                "console-run-run:fix-signup",
                "console-run-run:budget",
            ],
        ),
        (
            "run",
            &[
                "console-run-back",
                "console-run-stop",
                "console-run-take-over",
                "console-run-undo",
                "console-action-0",
                "console-action-1-view",
                "console-action-1-undo",
                "console-action-3",
            ],
        ),
        (
            "command",
            &["console-action-0-view", "console-action-0-view-open"],
        ),
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
fn a_program_or_a_page_offers_no_undo_and_a_stopped_agent_no_stop() {
    english();
    let mut app = TestApp::<App>::capture(Some("command")).expect("capture");
    assert!(
        !shows(&mut app, "console-action-0-undo"),
        "a program that ran changed no record to take back"
    );
    assert!(!shows(&mut app, "console-action-3-undo"));
    // Codex is not connected: its run is history, with nothing to stop.
    let mut app = TestApp::<App>::capture(Some("agents")).expect("capture");
    app.harness()
        .click_named("console-run-run:release-notes")
        .unwrap();
    app.frame();
    assert!(shows(&mut app, "console-run-undo"));
    assert!(!shows(&mut app, "console-run-stop"));
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

/// The request to `target` among `requests`; a missing one fails where the
/// test names it.
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
            },
        });
    app.frame();
}

fn agent() -> wire::Author {
    wire::Author {
        name: "Claude Code".into(),
        identity: "agent:claude".into(),
        relation: wire::Relation::Software,
        on_behalf_of: Some("Ada".into()),
        ..wire::Author::default()
    }
}

fn run() -> wire::AgentRun {
    let target = wire::Target {
        app_id: Some("so.tilo.notes".into()),
        id: "plan".into(),
        label: "Plan".into(),
        ..wire::Target::default()
    };
    wire::AgentRun {
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
        actions: vec![wire::RunAction {
            range: wire::Range::one("ab"),
            at_ms: 2,
            summary: "Rewrote the intro".into(),
            view: wire::ActionView::Record { target },
            app_id: "so.tilo.notes".into(),
            record: "record:plan".into(),
            undo: Some(wire::Offer {
                action: wire::DetailAction::UndoRecord,
                label: "Undo".into(),
            }),
        }],
        undo_blocked: None,
    }
}

fn the_door(connected: u32) -> door::Door {
    door::Door {
        endpoint: "https://mcp.example".into(),
        grants: vec![door::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            identity: "agent:claude".into(),
            connected,
            ..door::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// Open the console on a session that answers with `run` and a door where
/// its agent holds `connected` connections.
fn opened(connected: u32) -> TestApp<App> {
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let runs = find(&requests, &wire::target(wire::RUNS));
    let query: wire::RunsQuery = argument(&runs);
    assert_eq!(
        query.scope,
        wire::Scope::AllWorkspaces,
        "the console reads every workspace's runs"
    );
    let door_read = find(&requests, &door::target(door::DOOR));
    answer(
        &mut app,
        runs.invocation,
        serde_json::to_value(wire::Runs {
            runs: vec![run()],
            total: 1,
            pending: false,
            mutations: true,
        })
        .unwrap(),
    );
    answer(
        &mut app,
        door_read.invocation,
        serde_json::to_value(the_door(connected)).unwrap(),
    );
    app
}

#[test]
fn stopping_a_working_agent_asks_the_door_to_end_its_connections() {
    english();
    let mut app = opened(1);
    app.harness().click_named("console-run-r1").unwrap();
    asked(&mut app);
    app.harness().click_named("console-run-stop").unwrap();
    let requests = asked(&mut app);
    let stop: door::GrantRef = argument(&find(&requests, &door::target(door::STOP)));
    assert_eq!(stop.grant, "g1");
}

#[test]
fn undoing_a_run_or_one_action_asks_the_history_by_what_it_changed() {
    english();
    let mut app = opened(0);
    app.harness().click_named("console-run-r1").unwrap();
    asked(&mut app);
    assert!(
        !shows(&mut app, "console-run-stop"),
        "an agent that is not connected has nothing to stop"
    );

    app.harness().click_named("console-run-undo").unwrap();
    let requests = asked(&mut app);
    let range: wire::Range = argument(&find(&requests, &wire::target(wire::UNDO_RUN)));
    assert_eq!(range, run().range);

    app.harness().click_named("console-action-0-undo").unwrap();
    let requests = asked(&mut app);
    let one: wire::RecordQuery = argument(&find(&requests, &wire::target(wire::UNDO_RECORD)));
    assert_eq!(one.range, run().range, "the run it is part of");
    assert_eq!(
        (one.app_id.as_str(), one.id.as_str()),
        ("so.tilo.notes", "record:plan"),
        "the record the action changed, not the resource its view shows"
    );
}

#[test]
fn opening_an_action_shows_what_it_changed_and_reads_the_comparison() {
    english();
    let mut app = opened(0);
    app.harness().click_named("console-run-r1").unwrap();
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

#[test]
fn revoking_a_grant_names_it_to_the_door() {
    english();
    let mut app = opened(1);
    // Connect is the sidebar's third place.
    let targets: Vec<String> = app.harness().targets().into_keys().collect();
    let connect = targets
        .iter()
        .find(|target| target.ends_with("NavList[0]/NavRow[2]"))
        .cloned()
        .unwrap_or_else(|| panic!("the sidebar offers Connect among {targets:?}"));
    app.harness().click_named(&connect).unwrap();
    let requests = asked(&mut app);
    if let Some(read) = requests
        .iter()
        .find(|request| request.target == door::target(door::DOOR))
    {
        answer(
            &mut app,
            read.invocation,
            serde_json::to_value(the_door(1)).unwrap(),
        );
    }
    app.harness().click_named("console-grant-0-revoke").unwrap();
    let requests = asked(&mut app);
    let revoke: door::GrantRef = argument(&find(&requests, &door::target(door::REVOKE)));
    assert_eq!(revoke.grant, "g1");
}
