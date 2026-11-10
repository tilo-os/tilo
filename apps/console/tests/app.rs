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

/// Whether the window says `text`, as a name, value or description a screen
/// reader reads.
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

#[test]
fn every_scenario_shows_its_place_built_from_components() {
    english();
    let cases: [(&str, &[&str]); 7] = [
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
                "nav-shell-back",
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
            &[
                "console-action-0-view",
                "console-action-0-view-open",
                "console-action-0-output",
                "console-action-4",
            ],
        ),
        // The whole output of the failing run, read from its run record.
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
    let (claude, codex) = (at("Every Claude Code run"), at("Every Codex run"));
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
    assert!(shows(&mut app, "console-run-run:release-notes"));
    app.harness()
        .perform_named(
            "console-filter-agent",
            &ui::a11y::Action::SetValue("Claude Code".into()),
        )
        .unwrap();
    app.frame();
    assert!(shows(&mut app, "console-run-run:fix-signup"));
    assert!(shows(&mut app, "console-run-run:budget"));
    assert!(
        !shows(&mut app, "console-run-run:release-notes"),
        "Codex's run is another client's"
    );
}

#[test]
fn the_runs_back_shows_every_run_again() {
    english();
    let mut app = TestApp::<App>::capture(Some("run")).expect("capture");
    assert!(shows(&mut app, "console-run-undo"), "the run's own page");
    app.harness()
        .perform_named("nav-shell-back", &ui::a11y::Action::Activate)
        .unwrap();
    app.frame();
    assert!(shows(&mut app, "console-run-run:fix-signup"), "the runs");
    assert!(!shows(&mut app, "console-run-undo"));
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
    // Codex has no run open: its run is history, with nothing to stop.
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
        name: "Claude Code #2".into(),
        identity: "agent:claude".into(),
        relation: wire::Relation::Software,
        on_behalf_of: Some("Ada".into()),
        kind: ui::tilo_actions::ActorKind::Agent,
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
            changes: 1,
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
        now: Some(wire::Now::Running {
            command: "cargo test -p signup".into(),
            since_ms: 2,
        }),
    }
}

/// The door, with the run History names "Claude Code #2" open through it
/// or not.
fn the_door(open: bool) -> door::Door {
    door::Door {
        endpoint: "https://mcp.example".into(),
        grants: vec![door::Grant {
            id: "g1".into(),
            agent: "Claude Code".into(),
            runs: if open {
                vec![door::Run {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }]
            } else {
                Vec::new()
            },
            ..door::Grant::default()
        }],
        workspaces: Vec::new(),
    }
}

/// Open the console on a session that answers with `run` and a door where
/// that run is `open` or not.
fn opened(open: bool) -> TestApp<App> {
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
        serde_json::to_value(the_door(open)).unwrap(),
    );
    app
}

#[test]
fn stopping_a_working_run_asks_the_door_to_end_that_run_alone() {
    english();
    let mut app = opened(true);
    app.harness().click_named("console-run-r1").unwrap();
    asked(&mut app);
    app.harness().click_named("console-run-stop").unwrap();
    let requests = asked(&mut app);
    let stop: door::RunRef = argument(&find(&requests, &door::target(door::STOP)));
    assert_eq!(
        stop,
        door::RunRef {
            grant: "g1".into(),
            run: "Claude Code #2".into(),
        }
    );
}

#[test]
fn undoing_a_run_or_one_action_asks_the_history_by_what_it_changed() {
    english();
    let mut app = opened(false);
    app.harness().click_named("console-run-r1").unwrap();
    asked(&mut app);
    assert!(
        !shows(&mut app, "console-run-stop"),
        "a run whose session ended has nothing to stop"
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
    let mut app = opened(false);
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
    let mut app = opened(true);
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
            serde_json::to_value(the_door(true)).unwrap(),
        );
    }
    app.harness().click_named("console-grant-0-revoke").unwrap();
    let requests = asked(&mut app);
    let revoke: door::GrantRef = argument(&find(&requests, &door::target(door::REVOKE)));
    assert_eq!(revoke.grant, "g1");
}

/// A run that is working says what it is doing now on its row, from the
/// history's live read; once its session ended, the row names the agent
/// again rather than a command that is no longer running.
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
