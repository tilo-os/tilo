//! A task that works through tilolang programs: each `tilo` call is run by the session's api as
//! a step of the task's run, is recorded on the issue as a step with its answer, and a `view`
//! finish is checked by the Views host, kept by its `answer` action and recorded as the run's
//! answer.

use super::*;

/// A task asked on the canvas, whose programs read what its answer then shows there.
#[path = "canvas.rs"]
mod canvas;

const BOARD_PEER: u64 = 21;
const VIEWS_PEER: u64 = 22;
const CHAT_PEER: u64 = 23;
const API_PEER: u64 = 24;
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
        document_id: String::new(),
        live: true,
        actions,
    }
}

/// Board's and Views' hosts, Views' `answer`, and the tracker's step resources.
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
        resource_kind: format!("tl.{VIEWS}.Views"),
        resource_label: "Views".into(),
        ..descriptor(
            "answer",
            "Answer",
            tilo_actions::OperationKind::Document,
            vec![
                ActionParam::required("title", ParamType::Text, "title"),
                ActionParam::required("question", ParamType::Text, "question"),
                ActionParam::required("definition", ParamType::Text, "definition"),
                ActionParam::optional("shows", ParamType::Text, "shows"),
            ],
            Availability::Available,
            vec![],
        )
    });
    let chat = ExportedAction {
        resource: "chat.messages/messages/asked".into(),
        resource_kind: "chat.messages".into(),
        resource_label: "The conversation's reply".into(),
        ..descriptor(
            "record-work",
            "Record work",
            tilo_actions::OperationKind::Document,
            vec![
                ActionParam::required("message", ParamType::Text, "message"),
                ActionParam::required("work", ParamType::Object, "work"),
                ActionParam::optional("say", ParamType::Text, "say"),
            ],
            Availability::Available,
            vec![],
        )
    };
    let api = ExportedAction {
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
            ],
            Availability::Available,
            vec![],
        )
    };
    vec![
        surface(API_PEER, "so.tilo.agent", vec![api]),
        surface(BOARD_PEER, BOARD, host("Board")),
        surface(VIEWS_PEER, VIEWS, views),
        surface(CHAT_PEER, "so.tilo.chat", vec![chat]),
    ]
}

/// What the hosts declare: Board's types and Views' mounts.
pub(super) fn catalogue() -> Value {
    serde_json::json!([
        {"app": BOARD, "name": "Card", "signature": "type Card { title: Text, column: Column }"},
        {"app": BOARD, "name": "Column", "signature": "type Column { title: Text }"},
        {"app": BOARD, "name": "Board", "signature": "root Board"},
        {"app": BOARD, "name": "add_card", "signature": "action add_card(b: Board, col: Column, title: Text) -> Card"},
        {"app": VIEWS, "name": "WrittenView", "signature": "type WrittenView { title: Text, definition: Text }"},
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

/// What the conversation was asked to record, by action.
fn recorded(sent: &[CollabRequest], action: &str) -> Vec<Value> {
    sent.iter()
        .filter_map(invoked)
        .filter(|(_, invoked, _, _)| *invoked == action)
        .map(|(_, _, arguments, _)| Value::Object(arguments))
        .collect()
}

tilo_test_env::scenario! {
    #[allow(
        clippy::too_many_lines,
        reason = "one task end to end: its programs, their resources, and the view it answers with"
    )]
    fn a_task_works_in_programs_and_refuses_what_is_not_a_view;

    phase a_task_works_in_programs_and_answers_with_a_view;
    {
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
            // The view, checked by the Views host: a `view` declaration that takes nothing.
            tl_blocks: vec![(
                "row(c)",
                serde_json::json!({"kind": "view", "tree": {"kind": "column", "key": "0"},
                    "definition": {"name": "left", "shows": null, "params": []}}),
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
        let view = "view left() {\n    stack { for c in (root board.Board).cards { row(c) } }\n}";
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
        f.carried();
        let bodies = f.script.bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            4,
            "a turn per program and the finish; the last one read: {}",
            said_last(bodies.last().unwrap())
        );
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded {
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
        // The reply the run ends with lists its three programs, and nothing
        // else: the view it kept is its answer, not a step.
        let records = recorded(&sent, "record-work");
        let last = records
            .last()
            .expect("the reply was told how the run ended");
        let work = &last["work"];
        assert_eq!(work["state"], "provider_complete");
        let steps = work["trace"].as_array().expect("the reply's steps");
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert_eq!(steps[0]["outcome"], "refused", "{steps:?}");
        assert_eq!(steps[0]["resource"], "tilo.program");
        // A previewed program names the resource it read, and one that changed
        // something names the action it ran.
        assert_eq!(steps[1]["outcome"], "applied", "{steps:?}");
        assert_eq!(steps[1]["resource"], format!("tl:{BOARD}:root/cards:c1"));
        assert_eq!(steps[2]["outcome"], "applied", "{steps:?}");
        assert_eq!(steps[2]["label"], "move_card");
        assert_eq!(steps[2]["resource"], format!("tl:{BOARD}:root/cards:c1"));
        // Every program went to the API as a step of the turn, which the host names the session
        // of: the program itself names no group.
        let world = f.world.borrow();
        let ran: Vec<&serde_json::Map<String, Value>> = world
            .steps
            .iter()
            .filter(|(action, _)| action == tilo_actions::remote::task_programs::RUN)
            .map(|(_, arguments)| arguments)
            // What the task read of the apps' declarations (`describe`) is no program of the
            // model's.
            .filter(|arguments| {
                !arguments["program"]
                    .as_str()
                    .is_some_and(|program| program.starts_with("describe("))
            })
            .collect();
        assert_eq!(ran.len(), 3);
        assert!(ran[0].get("run").is_none(), "{:?}", ran[0]);
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
        // It is kept as the declaration the task wrote, among the person's own views.
        let answer = &world
            .steps
            .iter()
            .find(|(action, _)| action == "answer")
            .unwrap()
            .1;
        assert_eq!(answer["definition"], Value::String(view.to_owned()));
        assert_eq!(answer["shows"], Value::String(String::new()));
        drop(world);
        // The view is kept, and the reply that names it is the same one: no step
        // of the turn is the view itself.
        assert!(
            steps
                .iter()
                .all(|step| step["resource"] != format!("tl:{VIEWS}:root/items:v1"))
        );
    }

    phase a_view_that_is_not_a_view_is_refused_and_the_task_goes_on;
    {
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
        f.carried();
        let bodies = f.script.bodies.lock().unwrap();
        let refused = said_last(&bodies[1]);
        assert!(refused.contains("is not a `view` declaration"), "{refused}");
        drop(bodies);
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded {
                message: "There are 42.".into()
            }
        );
    }
}

/// "What changed in my notes this week": the model is told how a program asks by time before it
/// writes one. It first asks a note for a field it has not got; the error it reads back names
/// the place in its own program and the filter that answers it, and its next program uses that
/// filter and answers. The API's answers are the ones the task's host gives these programs
/// (`tilolang-tilo`'s `what_changed_in_the_notes_this_week_is_found_by_when_each_last_changed`).
#[test]
fn a_program_that_asks_a_field_for_when_a_note_changed_is_taught_the_filter_and_answers() {
    let wrong = "find(notes.Note).filter(n -> n.edited)";
    let right = "find(notes.Note, changed: -7d)";
    let taught = "1:32: E021 `Note` has no field `edited`; fields are: title, body, images, \
                  parent, pinned, tags, source, origin; when it was made or last changed, and by \
                  whom, is its history, not a field: `updated(r)` and `created(r)` read it, and \
                  `find(Note, changed: -7d)` finds those changed in the last 7 days";
    let world = World {
        tilolang: true,
        tl_programs: vec![
            (
                "n.edited",
                serde_json::json!({
                    "text": format!("error: the program does not compile; nothing ran\n{taught}"),
                    "failed": true,
                    "answer": {"state": "rejected", "detail": [taught]},
                }),
            ),
            (
                "changed: -7d",
                serde_json::json!({
                    "text": "n1 \"Groceries\"  pinned: true  tags: []",
                    "failed": false,
                    "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
                    "refs": [["n1", "@notes.Note:root/notes:g"]],
                }),
            ),
        ],
        ..World::default()
    };
    let mut f = fixture(
        "summarise what changed in my notes this week",
        world,
        vec![
            turn(&[tilo(wrong)], 64),
            turn(&[tilo(right)], 64),
            turn(
                &[(
                    tools::PLAN,
                    serde_json::json!({
                        "reply": "Only Groceries changed this week: you pinned it.",
                        "done": true
                    }),
                )],
                64,
            ),
        ],
    );
    f.carried();
    let bodies = f.script.bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "the wrong program, the right one, the finish"
    );
    // Before it writes a program, the model reads how one asks by time and what it answers with.
    let tool = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == crate::programs::TILO)
        .unwrap()
        .to_string();
    for taught in [
        "find(notes.Note, changed: -7d)",
        "read(n2)",
        "updated(n2)",
        "describe(notes)",
        "the value of its last line",
    ] {
        assert!(tool.contains(taught), "the tool teaches {taught}: {tool}");
    }
    // The error comes back whole: where in its program, and what answers instead.
    let second = said_last(&bodies[1]);
    assert!(second.contains("does not compile"), "{second}");
    assert!(second.contains("1:32: E021"), "{second}");
    assert!(second.contains("find(Note, changed: -7d)"), "{second}");
    let third = said_last(&bodies[2]);
    assert!(third.contains("n1 \"Groceries\""), "{third}");
    drop(bodies);
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "Only Groceries changed this week: you pinned it.".into()
        }
    );
}

/// "Remind me to call mum tomorrow at 6pm", done in one program: the program that made the task
/// did the part the model lists for Tasks, so the finish that lists it is taken at once. A part
/// done by a program used to stay undone, the finish was refused, and the model opened the Tasks
/// app's window to have "an operation in Tasks" — the whole app on the canvas, and a run that
/// ended as failed after it had made the task.
#[test]
fn a_part_done_by_a_program_lets_the_finish_through() {
    const TASKS: &str = "so.tilo.lang.tasks";
    let made = serde_json::json!({
        "text": "t1 \"Call mum\"  due: 2026-11-17  at: 18:00\nstep 1: 1 change in tasks; `undo` takes it back",
        "failed": false,
        "answer": {"state": "done", "output": [], "changed": 1, "apps": [TASKS],
            "steps": [{"app": TASKS, "action": "new_task",
                "resources": [format!("tl:{TASKS}:root/items:m")], "destructive": false}]},
        "refs": [["t1", "@tasks.Task:root/items:m"]],
    });
    let world = World {
        tilolang: true,
        tl_programs: vec![("new_task", made)],
        ..World::default()
    };
    let mut f = fixture(
        "remind me to call mum tomorrow at 6pm",
        world,
        vec![
            turn(
                &[tilo(
                    "tasks.new_task(tasks, title: \"Call mum\", due: 2026-11-17, at: 18:00)",
                )],
                64,
            ),
            turn(
                &[(
                    tools::PLAN,
                    serde_json::json!({
                        "reply": "I added \"Call mum\" for tomorrow at 18:00.",
                        "done": true,
                        "parts": [{"part": "a task to call mum", "app": TASKS}],
                    }),
                )],
                64,
            ),
        ],
    );
    f.carried();
    let bodies = f.script.bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "the program and the finish, no refusal; the last one read: {}",
        said_last(bodies.last().unwrap())
    );
    drop(bodies);
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "I added \"Call mum\" for tomorrow at 18:00.".into()
        }
    );
}

/// One response, one model call: the program and the reply the person reads once it has run.
/// "What's on my plate today?" is a program that prints the answer, and its reply is that
/// answer; nothing asks the model again to read it first.
#[test]
fn a_program_with_its_reply_finishes_in_one_call() {
    let today = serde_json::json!({
        "text": "- Book a haircut (overdue since 29 Jun)\n- Call the landlord about the boiler",
        "failed": false,
        "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
    });
    let world = World {
        tilolang: true,
        tl_programs: vec![("tasks.Task", today)],
        ..World::default()
    };
    let mut f = fixture(
        "what's on my plate today?",
        world,
        vec![turn(
            &[(
                crate::programs::TILO,
                serde_json::json!({
                    "program": "for t in find(tasks.Task, done: false) { print(\"- {t.title}\") }",
                    "reply": "Today:\n{answer}",
                }),
            )],
            64,
        )],
    );
    f.carried();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1, "one model call");
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "Today:\n- Book a haircut (overdue since 29 Jun)\n- Call the landlord about \
                      the boiler"
                .into()
        }
    );
}

/// A program that did not run to its end is read again: its reply is never told for it.
#[test]
fn a_failed_program_keeps_its_reply_back() {
    let failed = serde_json::json!({
        "text": "error: the program does not compile; nothing ran\n1:7: `tasks` has no `nope`",
        "failed": true,
        "answer": {"state": "rejected", "detail": ["1:7: `tasks` has no `nope`"]},
    });
    let world = World {
        tilolang: true,
        tl_programs: vec![("nope", failed)],
        ..World::default()
    };
    let mut f = fixture(
        "what's on my plate today?",
        world,
        vec![
            turn(
                &[(
                    crate::programs::TILO,
                    serde_json::json!({"program": "tasks.nope", "reply": "All done: {answer}"}),
                )],
                64,
            ),
            turn(&[finish("Nothing is due today.", true)], 64),
        ],
    );
    f.carried();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 2);
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "Nothing is due today.".into()
        }
    );
}

/// A run that keeps looking without getting further ("make a note with groceries" when nothing
/// writes a note's body: forty calls of describing and listing) is told after four such turns to
/// finish, and ends with the readable failure when that turn does not: five calls, not dozens.
#[test]
fn a_run_that_gets_no_further_is_told_to_finish_and_ends() {
    let looking = || turn(&[(tools::LIST_APPLICATIONS, serde_json::json!({}))], 64);
    let mut f = fixture(
        "make a note with the groceries",
        World::default(),
        (0..8).map(|_| looking()).collect(),
    );
    f.carried();
    let bodies = f.script.bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        5,
        "four idle turns and the one told to finish"
    );
    assert!(
        bodies[4].to_string().contains("Finish now"),
        "the fifth call is told to finish"
    );
    drop(bodies);
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Failed {
            message: crate::i18n::t("agent-failure-stalled")
        }
    );
}

/// What a program made, as the API answers a `new_note` that made "Groceries".
fn made_note() -> Value {
    const NOTES: &str = "so.tilo.lang.notes";
    serde_json::json!({
        "text": "n1 \"Groceries\"\nstep 1: 1 change in notes; `undo` takes it back",
        "failed": false,
        "answer": {"state": "done", "output": [], "changed": 1, "apps": [NOTES],
            "steps": [{"app": NOTES, "action": "new_note",
                "resources": [format!("tl:{NOTES}:root/notes:n")], "destructive": false}]},
        "refs": [["n1", "@notes.Note:root/notes:n"]],
    })
}

/// A request's first step may be a program, and the separate things the request asks for are
/// listed on its first step: a program that carries them with its reply is the whole answer in
/// one call. `parts` on a program used to refuse the whole turn ("unexpected argument parts").
#[test]
fn a_program_that_lists_the_parts_finishes_in_one_call() {
    let world = World {
        tilolang: true,
        tl_programs: vec![("new_note", made_note())],
        ..World::default()
    };
    let mut f = fixture(
        "make a note with the groceries: eggs, milk, bread",
        world,
        vec![turn(
            &[(
                crate::programs::TILO,
                serde_json::json!({
                    "program": "notes.new_note(notes.notebook, title: \"Groceries\", body: \"- eggs\")",
                    "confirm": true,
                    "parts": [{"part": "a groceries note", "app": "so.tilo.lang.notes"}],
                    "reply": "Made the note Groceries.",
                }),
            )],
            64,
        )],
    );
    f.carried();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1, "one model call");
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "Made the note Groceries.".into()
        }
    );
}

/// A program that made the change and carried no reply is followed by the words alone: the
/// model reads that nothing is left to run, never a program answer it takes for a cue to run
/// another (`print("ok")` with "ok" for its reply).
#[test]
fn a_change_without_its_reply_is_told_to_finish() {
    let world = World {
        tilolang: true,
        tl_programs: vec![("new_note", made_note())],
        ..World::default()
    };
    let mut f = fixture(
        "make a note with the groceries: eggs, milk, bread",
        world,
        vec![
            turn(
                &[(
                    crate::programs::TILO,
                    serde_json::json!({
                        "program": "notes.new_note(notes.notebook, title: \"Groceries\")",
                        "confirm": true,
                    }),
                )],
                64,
            ),
            turn(&[finish("Made the note Groceries.", true)], 64),
        ],
    );
    f.carried();
    let read = {
        let bodies = f.script.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        said_last(&bodies[1])
    };
    assert!(read.contains("Nothing is left to run"), "{read}");
}

/// A question is done by its answer: a program that read the board and printed what is on it did
/// the part the model listed for the board, so its reply finishes the turn in the one call.
/// A program that changed nothing used to leave the part undone and the finish refused ("these
/// parts of the request have no successful operation yet").
#[test]
fn a_question_answered_by_a_program_does_its_part() {
    let read = serde_json::json!({
        "text": "- Write the launch post\n- Fix the sync bug",
        "failed": false,
        "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
    });
    let world = World {
        tilolang: true,
        tl_programs: vec![("board.Card", read)],
        ..World::default()
    };
    let mut f = fixture(
        "what is on the board?",
        world,
        vec![turn(
            &[(
                crate::programs::TILO,
                serde_json::json!({
                    "program": "for c in find(board.Card) { print(\"- {c.title}\") }",
                    "parts": [{"part": "what is on the board", "app": BOARD}],
                    "reply": "{answer}",
                }),
            )],
            64,
        )],
    );
    f.carried();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1, "one model call");
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Succeeded {
            message: "- Write the launch post\n- Fix the sync bug".into()
        }
    );
}

/// A reply on a program that only reads does not answer a request to change something. The
/// request "add a card saying Friday to the board" declares that card as its part; a program that
/// counted the board's cards and said "Checking what is on the board." changed nothing there.
/// The step was counted as doing the part — a program that changed nothing still credited every
/// app it named — so the turn ended succeeded with that sentence while the card was never added,
/// and the person read an answer to a request nobody carried out. A program that changed nothing
/// does no part of a request to do something, and a reply that rides a step leaving a declared part
/// undone does not end the turn: the model reads what is left and does it.
#[test]
fn a_reply_on_a_program_that_leaves_a_part_undone_does_not_end_the_turn() {
    let read = serde_json::json!({
        "text": "c1 \"Write the release notes\" column: c2",
        "failed": false,
        "answer": {"state": "done", "output": [], "steps": [], "changed": 0, "apps": []},
    });
    let world = World {
        tilolang: true,
        tl_programs: vec![("board.Card", read)],
        ..World::default()
    };
    let mut f = fixture(
        "add a card saying Friday to the board",
        world,
        vec![
            turn(
                &[(
                    crate::programs::TILO,
                    serde_json::json!({
                        "program": "for c in find(board.Card) { print(c.title) }",
                        "parts": [{"part": "a card saying Friday", "app": BOARD}],
                        "reply": "Checking what is on the board.",
                    }),
                )],
                64,
            ),
            turn(&[finish("The board holds no such card.", false)], 64),
        ],
    );
    f.carried();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        2,
        "the read-only program's reply did not finish the turn; the model was read what is left"
    );
    let second = bodies[1]["messages"].to_string();
    assert!(
        second.contains("Not finished") && second.contains("a card saying Friday"),
        "the model read that its part is not done: {second}"
    );
    // The person reads the model's own words, never "Checking what is on the board." while the
    // card it said it would add was never added.
    assert_eq!(
        terminal(&f.sent.borrow()),
        TurnUpdate::Failed {
            message: "The board holds no such card.".into()
        }
    );
}
