//! The conversation on the native session (shell.md §8.1): the agent chat and
//! the REPL in one shell view, over a run of the session's agent. The view
//! draws the run the session publishes: what the person asked, the agent's
//! words between its steps, and each program as a step with what it printed
//! as references.
//!
//! The test plays the agent's side of the run, as the broker would deliver its
//! reports and the agent its steps (`tests/agent_runs.rs`); the ask and the
//! surface go through the session's door, as the shell's own presses do.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::{ActionAddress, ActionSpec, OperationKind, ResourceSpec, ResourceViewSpec};
use tilo_agent_runs::{AgentRuns, wire};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_reference::{ProgramStep, ResourceAt, ResourceRef, StepOutcome};
use tilo_service::LoopWake;
use tilo_session::{Options, Session};
use tilo_surface_host::shell_part::DESK_RESOURCE;
use tilo_surface_host::{DeskReading, Published};

const PERSON: &str = "Pat";
const NOTES: &str = "so.tilo.lang.notes";
const BTN_LEFT: u32 = 0x110;

/// A headless session with the shell over it, and the runs whose agent's
/// side this test plays.
fn session(runs: &Arc<AgentRuns>) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            wake: LoopWake::default(),
            agent_runs: Some(Arc::clone(runs)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session")
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    common::until(session, "what the test asked for", done);
}

fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label == words)
}

/// The desk as the session publishes it on its door.
fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(session.read(DESK_RESOURCE).expect("the desk")).expect("a desk")
}

fn arguments(value: Value) -> Map<String, Value> {
    let Value::Object(arguments) = value else {
        panic!("arguments are an object: {value}")
    };
    arguments
}

/// The person asks the agent through the door, as the composer does; the run
/// it started.
fn ask(session: &mut Session, title: &str) -> String {
    let started = session
        .invoke(
            wire::RESOURCE,
            "ask",
            &arguments(json!({ "id": "tidy", "title": title, "agent": "agent:claude" })),
        )
        .expect("the session queues the ask");
    assert!(
        matches!(
            started,
            ui::host_operation::ResourceOperationStart::Completed(_)
        ),
        "the ask is queued at once: {started:?}"
    );
    "tidy".to_owned()
}

/// A note, as a step names what it printed.
fn note(id: &str, title: &str) -> ResourceAt {
    ResourceAt {
        app: "so.tilo.lang.notes".into(),
        record: None,
        reference: ResourceRef::whole("notes.Note", format!("tl:so.tilo.lang.notes:{id}"), title),
    }
}

/// The run's agent side: it takes the run up, says what it will do, runs a program that prints
/// two notes, and answers.
fn the_agent_finds_the_launch_notes(runs: &AgentRuns, run: &str) {
    let workspace = runs.front();
    let issue = wire::issue(run);
    runs.absorb_report(
        &workspace,
        &issue,
        1,
        &runtime_collab::TaskRunUpdate::Accepted,
    );
    runs.absorb_report(
        &workspace,
        &issue,
        1,
        &runtime_collab::TaskRunUpdate::Progress {
            message: "Looking for the launch notes first".into(),
            presentation: None,
        },
    );
    runs.record_step(
        run,
        ProgramStep {
            program: "find(notes.Note, \"launch\")".into(),
            outcome: StepOutcome::Done,
            answer: vec!["n1 \"Launch checklist\"".into(), "n2 \"Launch day\"".into()],
            refs: vec![note("n1", "Launch checklist"), note("n2", "Launch day")],
            ..ProgramStep::default()
        },
    )
    .expect("the agent records its step");
    runs.absorb_report(
        &workspace,
        &issue,
        1,
        &runtime_collab::TaskRunUpdate::Succeeded {
            message: "Two launch notes: the checklist and the day plan".into(),
        },
    );
}

/// Show the conversation over the desk, as the shell's key for it does.
fn show_conversation(session: &mut Session) {
    session
        .invoke(
            DESK_RESOURCE,
            "show-surface",
            &arguments(json!({ "surface": "conversation" })),
        )
        .unwrap();
    until(session, |session| desk(session).here == "conversation");
}

/// Press the shell's control named `name` with the pointer.
fn press(session: &mut Session, name: &str) {
    let (x, y, w, h) = session
        .shell_control(name)
        .unwrap_or_else(|| panic!("the shell shows {name}: {:?}", session.accessible_labels()));
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// The notes the agent's step printed, published into the session's door as Notes publishes
/// them: a stand-in for the Notes app, which the headless session does not run, publishing
/// only what the arrangement reads of a resource (its app and its primary view).
struct LaunchNotes;

impl Published for LaunchNotes {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        let open = ActionSpec::typed_operation(
            "open-note",
            "Open".to_owned(),
            String::new(),
            OperationKind::View,
            "test.notes",
            Vec::new(),
            |_| {
                tilo_actions::EncodedAction::encode_operation("test.notes", &"open-note")
                    .expect("a view action always serializes")
            },
        );
        let mut app = ResourceSpec::new(format!("tl:{NOTES}"), "notes.app", "Notes");
        app.app_id = Some(NOTES.to_owned());
        let notes = [("n1", "Launch checklist"), ("n2", "Launch day")].map(|(id, title)| {
            let mut note = ResourceSpec::new(format!("tl:{NOTES}:{id}"), "notes.Note", title)
                .with_action(open.clone())
                .with_view(ResourceViewSpec::new("note", "Note", "open-note").with_primary(true));
            note.app_id = Some(NOTES.to_owned());
            note
        });
        vec![app.with_children(notes)]
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        _: &Map<String, Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unknown(address.clone()))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

#[test]
fn the_conversation_draws_the_runs_words_and_steps_in_order() {
    let _settled = ui::motion::testing::settled();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = session(&runs);
    // The session tells the runs who sits here and the workspace in front.
    until(&mut session, |_| !runs.front().is_empty());
    let run = ask(&mut session, "Tidy the launch notes");
    the_agent_finds_the_launch_notes(&runs, &run);

    // The desk publishes the run's conversation, entry by entry, with what
    // the step printed.
    until(&mut session, |session| desk(session).run_entries.len() == 4);
    let read = desk(&session);
    let entries: Vec<(bool, bool, &str)> = read
        .run_entries
        .iter()
        .map(|entry| (entry.by_agent, entry.step, entry.text.as_str()))
        .collect();
    assert_eq!(
        entries,
        vec![
            (false, false, "Tidy the launch notes"),
            (true, false, "Looking for the launch notes first"),
            (true, true, "find(notes.Note, \"launch\")"),
            (
                true,
                false,
                "Two launch notes: the checklist and the day plan"
            ),
        ],
        "what was said and run, in the order it happened"
    );
    let refs: Vec<&str> = read
        .entry_refs
        .iter()
        .map(|reference| reference.resource.as_str())
        .collect();
    assert_eq!(
        refs,
        vec!["tl:so.tilo.lang.notes:n1", "tl:so.tilo.lang.notes:n2"],
        "the step's references, by the resource the arrangement places"
    );
    assert!(
        read.entry_refs
            .iter()
            .all(|reference| reference.entry == read.run_entries[2].key),
        "each names the step it is of"
    );

    // The conversation is a surface of the desk: the shell draws it.
    show_conversation(&mut session);
    for words in [
        "Tidy the launch notes",
        "Looking for the launch notes first",
        "Launch checklist",
        "Launch day",
        "Two launch notes: the checklist and the day plan",
        "find(notes.Note, \"launch\")",
    ] {
        until(&mut session, |session| says(session, words));
    }
    let read = desk(&session);
    assert_eq!(
        read.runs
            .iter()
            .map(|run| (run.title.as_str(), run.phase.as_str()))
            .collect::<Vec<_>>(),
        vec![("Tidy the launch notes", "done")],
        "the run, under its title, where it stands"
    );
}

/// Activating a reference a step printed puts the resource on the canvas, through the
/// arrangement's `place` action: an entry naming the resource, shown by its app, standing in a
/// strip of its own on the workspace in front, as the person's one step of the record.
#[test]
fn pressing_a_reference_places_its_resource_on_the_canvas() {
    let _settled = ui::motion::testing::settled();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = session(&runs);
    session.publish(Rc::new(RefCell::new(LaunchNotes)));
    until(&mut session, |_| !runs.front().is_empty());
    let run = ask(&mut session, "Tidy the launch notes");
    the_agent_finds_the_launch_notes(&runs, &run);
    show_conversation(&mut session);
    until(&mut session, |session| says(session, "Launch checklist"));

    press(&mut session, "Launch checklist");
    let placed = |session: &Session| {
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .find(|place| place.record == format!("tl:{NOTES}:n1"))
            .cloned()
    };
    until(&mut session, |session| placed(session).is_some());
    let place = placed(&session).unwrap();
    assert_eq!(place.shows, NOTES, "shown by the app that keeps it");
    let held = session.arrangement().borrow().strips();
    let strip = held
        .strip_of(place.id)
        .expect("it stands in a strip of the record");
    assert!(strip.canvas.is_some(), "it stands on the canvas");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!((who.as_str(), verb.as_str()), (PERSON, "place"));
}

/// A run placed on the canvas is a place like any other: the arrangement's `place` action names
/// the run's resource, and the place is the shell's, through the run's one view, the
/// conversation. (The shell's canvas draws that place as the conversation:
/// `tilolang-tilo`'s `shell_record::a_placed_run_stands_on_the_canvas_as_its_conversation`.)
#[test]
fn a_run_placed_on_the_canvas_is_the_shells_conversation_place() {
    let _settled = ui::motion::testing::settled();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = session(&runs);
    until(&mut session, |_| !runs.front().is_empty());
    let run = ask(&mut session, "Tidy the launch notes");
    the_agent_finds_the_launch_notes(&runs, &run);
    until(&mut session, |session| desk(session).run_entries.len() == 4);
    let resource = wire::run(&run);

    session
        .invoke(
            "shell:arrangement",
            "place",
            &arguments(json!({ "resource": resource, "x": 40, "y": 40 })),
        )
        .expect("the arranging action runs");
    let placed = |session: &Session| {
        session
            .arrangement()
            .borrow()
            .strips()
            .places()
            .find(|place| place.record == resource)
            .cloned()
    };
    until(&mut session, |session| placed(session).is_some());
    let place = placed(&session).unwrap();
    assert_eq!(place.shows, "so.tilo.shell", "the shell shows a run");
    assert_eq!(place.view, "conversation", "through its conversation");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!((who.as_str(), verb.as_str()), (PERSON, "place"));
}

// ---- the conversation's one input, two ways in (shell.md §8.1) ---------------------------
//
// The canvas entry and the conversation's own field both press `shell:arrangement`'s
// `conversation` with the line the person typed. A line that parses as a tilolang program runs
// as the person in the run's program host; anything else is a question that starts a run, whose
// conversation stands where they typed.

/// What the conversation's action answers: the line, where a new run's conversation stands, and
/// the run a program or a sentence is in.
fn converse(session: &mut Session, line: &str, run: &str, at: (i64, i64)) -> Result<(), String> {
    let mut arguments = Map::new();
    arguments.insert("line".to_owned(), Value::String(line.to_owned()));
    if !run.is_empty() {
        arguments.insert("run".to_owned(), Value::String(run.to_owned()));
    }
    arguments.insert("x".to_owned(), Value::from(at.0));
    arguments.insert("y".to_owned(), Value::from(at.1));
    session
        .invoke("shell:arrangement", "conversation", &arguments)
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

/// The place of the arrangement that shows `record`, if it stands.
fn placed_record(
    session: &Session,
    record: &str,
) -> Option<tilo_desktop_model::own_arrangement::Place> {
    session
        .arrangement()
        .borrow()
        .strips()
        .places()
        .find(|place| place.record == record)
        .cloned()
}

/// A question typed at the canvas starts a run, and its conversation stands where the person
/// typed: an entry naming the run, placed through the arrangement's own `place`, as one step of
/// the person's.
#[test]
fn a_sentence_typed_at_the_canvas_places_its_conversation() {
    let _settled = ui::motion::testing::settled();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = session(&runs);
    // The session tells the runs who sits here and the workspace in front.
    until(&mut session, |_| !runs.front().is_empty());

    converse(&mut session, "Tidy the launch notes", "", (120, 80))
        .expect("the sentence starts a run");
    until(&mut session, |_| runs.readings().len() == 1);
    let started = runs.readings()[0].id.clone();
    let resource = wire::run(&started);
    until(&mut session, |session| {
        placed_record(session, &resource).is_some()
    });

    let place = placed_record(&session, &resource).expect("the conversation stands");
    assert_eq!(place.shows, "so.tilo.shell", "the shell shows a run");
    assert_eq!(place.view, "conversation", "through its conversation");
    let held = session.arrangement().borrow().strips();
    let strip = held.strip_of(place.id).expect("it stands in a strip");
    let canvas = strip.canvas.expect("it stands on the canvas");
    assert_eq!((canvas.x, canvas.y), (120, 80), "where the person typed");
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("it is a step of the record's history");
    assert_eq!(
        (who.as_str(), verb.as_str()),
        (PERSON, "place"),
        "the person's own step"
    );
    assert_eq!(
        runs.readings()[0].transcript[0].author,
        PERSON,
        "the run starts with the person's words"
    );
}

/// A line that parses as a program runs as the person, in the run's own program host, and the
/// conversation shows it as their step. The host is the door's, so the program reads and
/// changes as the person does.
#[test]
fn a_program_line_runs_as_the_person_and_is_a_step() {
    let _settled = ui::motion::testing::settled();
    let runs = Arc::new(AgentRuns::new(None));
    let mut session = session(&runs);
    session.publish(Rc::new(RefCell::new(LaunchNotes)));
    until(&mut session, |_| !runs.front().is_empty());
    let run = ask(&mut session, "Tidy the launch notes");
    // The run is going, and its host starts on its first program: the person's line is that
    // program here, so the run needs no agent turn before it.
    // A line that does not parse as a program is a sentence; one that does is a program.
    assert!(
        tilo_session::conversation::parses("find(notes.Note)"),
        "a program's line parses"
    );
    assert!(
        !tilo_session::conversation::parses("tidy the launch notes"),
        "a sentence is no program"
    );

    converse(&mut session, "1 + 1", &run, (0, 0)).expect("the program runs");
    // The line runs on a thread of its own; the session records its step on the run when it
    // ends, and the desk shows it on its next pass.
    until(&mut session, |session| {
        desk(session)
            .run_entries
            .iter()
            .any(|entry| entry.step && entry.text == "1 + 1")
    });
    let read = desk(&session);
    let step = read
        .run_entries
        .iter()
        .find(|entry| entry.text == "1 + 1")
        .expect("the program is a step");
    assert!(!step.by_agent, "the person's step, not the agent's");
    assert_eq!(step.outcome, "done", "it ran to its end");
}
