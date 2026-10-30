//! A task that works through tilolang programs: each `tilo` call is run by the session's door as
//! a step of the task's run, is recorded on the issue as a step with its answer, and a `view`
//! finish is checked by the Views host, kept by its `answer` action and recorded as the run's
//! answer.

use super::*;
use ui::tilo_reference::{ProgramStep, ResourceAt, StepOutcome};

const BOARD_PEER: u64 = 21;
const VIEWS_PEER: u64 = 22;
const TRACKER_PEER: u64 = 23;
const DOOR_PEER: u64 = 24;
const BOARD: &str = "so.tilo.lang.board";
const VIEWS: &str = "so.tilo.lang.views";

fn host_action(action: &str, label: &str, params: Vec<ActionParam>) -> ExportedAction {
    ExportedAction {
        resource: crate::programs::HOST_RESOURCE.into(),
        resource_kind: "tl.host".into(),
        resource_label: "Host".into(),
        ..descriptor(
            action,
            label,
            tilo_actions::OperationKind::Effect,
            params,
            Availability::Available,
            vec![],
        )
    }
}

fn surface(peer: u64, app: &str, actions: Vec<ExportedAction>) -> BrokerActionSurface {
    BrokerActionSurface {
        peer_id: peer,
        provider: None,
        app_id: app.into(),
        cluster_id: Uuid::from_u128(1).to_string(),
        record_id: String::new(),
        live: true,
        actions,
    }
}

/// Board's and Views' hosts, Views' `answer`, and the tracker's step records.
pub(super) fn surfaces() -> Vec<BrokerActionSurface> {
    let source = || {
        vec![ActionParam::required(
            "source",
            ParamType::Text,
            "The block's source",
        )]
    };
    let host = |app: &str| {
        vec![
            host_action(crate::programs::RUN_BLOCK, "Run a block", source()),
            host_action(crate::programs::CATALOGUE, "List the catalogue", vec![]),
        ]
        .into_iter()
        .map(|mut action| {
            action.resource_label = format!("{app} host");
            action
        })
        .collect::<Vec<_>>()
    };
    let mut views = host("Views");
    views.push(ExportedAction {
        resource: format!("tl:{VIEWS}:root"),
        resource_kind: format!("tl.{VIEWS}.Answers"),
        resource_label: "Views".into(),
        ..descriptor(
            "answer",
            "Answer",
            tilo_actions::OperationKind::Document,
            vec![
                ActionParam::required("title", ParamType::Text, "title"),
                ActionParam::required("question", ParamType::Text, "question"),
                ActionParam::required("source", ParamType::Text, "source"),
            ],
            Availability::Available,
            vec![],
        )
    });
    let tracker = |action: &'static str, param: &'static str| ExportedAction {
        resource: "tasks.tracker".into(),
        resource_kind: "tasks.tracker".into(),
        resource_label: "Tasks".into(),
        ..descriptor(
            action,
            action,
            tilo_actions::OperationKind::Document,
            vec![
                ActionParam::required("issue", ParamType::Text, "issue"),
                ActionParam::required(param, ParamType::Object, param),
            ],
            Availability::Available,
            vec![],
        )
    };
    let door = ExportedAction {
        resource: tilo_actions::remote::task_programs::RESOURCE.into(),
        resource_kind: tilo_actions::remote::task_programs::RESOURCE.into(),
        resource_label: "Task programs".into(),
        ..descriptor(
            tilo_actions::remote::task_programs::RUN,
            "Run a step of a task",
            tilo_actions::OperationKind::View,
            vec![
                ActionParam::required("program", ParamType::Text, "program"),
                ActionParam::optional("confirm", ParamType::Boolean, "confirm"),
                ActionParam::optional("this", ParamType::Text, "this"),
                ActionParam::required("run", ParamType::Text, "run"),
                ActionParam::optional("label", ParamType::Text, "label"),
            ],
            Availability::Available,
            vec![],
        )
    };
    vec![
        surface(DOOR_PEER, "so.tilo.agent", vec![door]),
        surface(BOARD_PEER, BOARD, host("Board")),
        surface(VIEWS_PEER, VIEWS, views),
        surface(
            TRACKER_PEER,
            "so.tilo.tasks",
            vec![
                tracker("record-step", "step"),
                tracker("record-answer", "view"),
            ],
        ),
    ]
}

/// What the hosts declare: Board's types and Views' mounts.
pub(super) fn catalogue() -> Value {
    serde_json::json!([
        {"app": BOARD, "name": "Card", "signature": "type Card { title: Text, column: Column }"},
        {"app": BOARD, "name": "Column", "signature": "type Column { title: Text }"},
        {"app": BOARD, "name": "Board", "signature": "root Board"},
        {"app": BOARD, "name": "add_card", "signature": "action add_card(b: Board, col: Column, title: Text) -> Card"},
        {"app": VIEWS, "name": "Answer", "signature": "type Answer { title: Text, source: Text }"},
    ])
}

/// What the last tool result of a request said, as the model read it.
fn said_last(body: &Value) -> String {
    fn texts(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
            Value::Object(map) => map.values().for_each(|item| texts(item, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    texts(
        body["messages"].as_array().unwrap().last().unwrap(),
        &mut out,
    );
    out
}

fn tilo(program: &str) -> (&'static str, Value) {
    (
        crate::programs::TILO,
        serde_json::json!({ "program": program }),
    )
}

/// What the tracker was asked to record, by action.
fn recorded(sent: &[CollabRequest], action: &str) -> Vec<Value> {
    sent.iter()
        .filter_map(|request| match request {
            CollabRequest::InvokeTaskAction {
                action: invoked,
                arguments,
                ..
            } if invoked == action => Some(Value::Object(arguments.clone())),
            _ => None,
        })
        .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one task end to end: its programs, their records, and the view it answers with"
)]
fn a_task_works_in_programs_and_answers_with_a_view() {
    let cards = serde_json::json!({
        "text": "c1 \"Write the release notes\" column: c2",
        "failed": false,
        "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
        "refs": [["c1", "@board.Card:root/cards:c1"], ["c2", "@board.Column:root/columns:k1"]],
    });
    let moved = serde_json::json!({
        "text": "step 1: 1 change in board; `undo` takes it back",
        "failed": false,
        "answer": {"state": "done", "output": [], "changed": 1, "apps": [BOARD],
            "steps": [{"app": BOARD, "action": "move_card",
                "resources": [format!("tl:{BOARD}:root/cards:c1")], "destructive": false}]},
    });
    let world = World {
        tilolang: true,
        // The view's rows, checked by the Views host.
        tl_blocks: vec![(
            "row(c)",
            serde_json::json!({"kind": "view", "tree": {"kind": "column", "key": "0"}}),
        )],
        // Matched in order: the program that does not compile, the move, the cards.
        tl_programs: vec![
            (
                "nope",
                serde_json::json!({
                    "text": "error: the program does not compile; nothing ran\n1:7: `board` has no `nope`",
                    "failed": true,
                    "answer": {"state": "rejected", "detail": ["1:7: `board` has no `nope`"]},
                }),
            ),
            ("move", moved),
            ("board.Card", cards),
        ],
        ..World::default()
    };
    let view = "column { for c in (root board.Board).cards { row(c) } }";
    let mut f = fixture(
        "what's left for the release?",
        world,
        vec![
            turn(&[tilo("board.nope")], 64),
            turn(
                &[tilo(
                    "find(board.Card).filter(c -> c.column.title != \"Done\")",
                )],
                64,
            ),
            turn(&[tilo("board.move_card(c1, c2)")], 64),
            turn(
                &[(
                    tools::PLAN,
                    serde_json::json!({
                        "reply": "One card is left.",
                        "done": true,
                        "view": {"title": "Left for the release", "source": view}
                    }),
                )],
                64,
            ),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        4,
        "a turn per program and the finish; the last one read: {}",
        said_last(bodies.last().unwrap())
    );
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "One card is left.".into()
        }
    );
    // The first message says where the task is: the apps its programs reach, and no limits
    // when its person set none.
    let first = bodies[0].to_string();
    assert!(!first.contains("Limits:"), "{first}");
    assert!(first.contains("`board`: Board"), "{first}");
    assert!(first.contains("types Card, Column"), "{first}");
    assert!(tool_names(&bodies[0]).contains(&crate::programs::TILO.to_owned()));
    // A program that does not compile answers what is valid; the next one answers in tilolang.
    let second = bodies[1].to_string();
    assert!(second.contains("does not compile"), "{second}");
    let third = said_last(&bodies[2]);
    assert!(third.contains("c1 \"Write the release notes\""), "{third}");
    drop(bodies);
    let sent = f.sent.borrow();
    let steps: Vec<ProgramStep> = recorded(&sent, "record-step")
        .into_iter()
        .map(|arguments| serde_json::from_value(arguments["step"].clone()).unwrap())
        .collect();
    assert_eq!(steps.len(), 3, "{steps:?}");
    assert_eq!(steps[0].outcome, StepOutcome::Rejected);
    assert_eq!(steps[1].outcome, StepOutcome::Done);
    assert_eq!(steps[1].refs[0].app, BOARD);
    assert_eq!(
        steps[1].refs[0].reference.id,
        format!("tl:{BOARD}:root/cards:c1")
    );
    assert_eq!(steps[1].refs[0].reference.label, "Write the release notes");
    assert_eq!(steps[1].refs[0].reference.kind, format!("tl.{BOARD}.Card"));
    // A program that changed something is a row that undoes it.
    assert_eq!(steps[2].changes.len(), 1, "{:?}", steps[2]);
    assert_eq!(steps[2].changes[0].action, "move_card");
    assert_eq!(
        steps[2].changes[0].resource.reference.id,
        format!("tl:{BOARD}:root/cards:c1")
    );
    // Every program went to the door as a step of the task's run.
    let world = f.world.borrow();
    let ran: Vec<&serde_json::Map<String, Value>> = world
        .steps
        .iter()
        .filter(|(action, _)| action == tilo_actions::remote::task_programs::RUN)
        .map(|(_, arguments)| arguments)
        .collect();
    assert_eq!(ran.len(), 3);
    assert_eq!(ran[0]["run"], Value::String(f.run.group()));
    drop(world);
    // The view was checked by the Views host before it was kept, and the answer names the
    // resource keeping it.
    let world = f.world.borrow();
    let kept: Vec<&str> = world
        .steps
        .iter()
        .map(|(action, _)| action.as_str())
        .filter(|action| ["run-block", "answer"].contains(action))
        .collect();
    assert_eq!(kept, ["run-block", "answer"]);
    let answers: Vec<ResourceAt> = recorded(&sent, "record-answer")
        .into_iter()
        .map(|arguments| serde_json::from_value(arguments["view"].clone()).unwrap())
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].app, VIEWS);
    assert_eq!(answers[0].reference.id, format!("tl:{VIEWS}:root/items:v1"));
    assert_eq!(answers[0].reference.label, "Left for the release");
}

#[test]
fn a_view_that_is_not_a_view_is_refused_and_the_task_goes_on() {
    let world = World {
        tilolang: true,
        tl_blocks: vec![("42", serde_json::json!({"kind": "data", "value": 42}))],
        ..World::default()
    };
    let mut f = fixture(
        "how many?",
        world,
        vec![
            turn(
                &[(
                    tools::PLAN,
                    serde_json::json!({"reply": "42", "done": true,
                        "view": {"title": "Count", "source": "42"}}),
                )],
                64,
            ),
            turn(&[finish("There are 42.", true)], 64),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap();
    let refused = said_last(&bodies[1]);
    assert!(refused.contains("is not a view"), "{refused}");
    drop(bodies);
    assert!(recorded(&f.sent.borrow(), "record-answer").is_empty());
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "There are 42.".into()
        }
    );
}
