//! The runs on their own: what an ask becomes, what a follower reads as its
//! agent reports and streams, and what the broker reads of it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step is the assertion"
)]

use std::sync::Arc;

use tilo_actions::DoorError;
use tilo_reference::{ProgramStep, ResourceAt};

use crate::{AgentRuns, Ask, wire};

/// One program an agent ran, as its run records it.
fn program(source: &str) -> ProgramStep {
    ProgramStep {
        program: source.into(),
        ..ProgramStep::default()
    }
}

/// The view a run answered with, in one of the person's notes.
fn view() -> ResourceAt {
    ResourceAt {
        app: "so.tilo.notes".into(),
        record: None,
        reference: tilo_reference::ResourceRef {
            kind: "notes.note".into(),
            id: "n-1".into(),
            label: String::new(),
            span: None,
        },
    }
}

const ROOM: &str = "room-1";

/// Runs of a session whose person asked in `room`, with `agents` admitted.
fn runs(agents: &[&str]) -> Arc<AgentRuns> {
    let runs = Arc::new(AgentRuns::new(None));
    runs.here("ada", ROOM, vec![room(ROOM, agents)]);
    runs
}

/// `room`, admitting `agents`.
fn room(room: &str, agents: &[&str]) -> crate::RoomFacts {
    crate::RoomFacts::new(
        room,
        agents.iter().map(|agent| (*agent).to_owned()).collect(),
        None,
    )
}

fn ask(title: &str, agent: &str) -> Ask {
    Ask {
        id: Some("next".into()),
        title: title.into(),
        body: None,
        agent: agent.into(),
        model: None,
        subject: None,
        quick: None,
    }
}

#[test]
fn an_ask_is_a_run_a_follower_reads() {
    let runs = runs(&["claude"]);
    let asked = runs
        .ask(ask("Tidy the notes", "agent:claude"))
        .expect("the ask");
    assert_eq!(asked.run, "agent-run:next");
    assert_eq!(
        asked.issue, "tasks.tracker/issues/next",
        "the run channel names the run by the work it does"
    );
    let run = runs.reading("next").expect("the run it started");
    assert_eq!(run.title, "Tidy the notes");
    assert_eq!(
        run.instruction, "Tidy the notes",
        "an ask with no body asks its title"
    );
    assert_eq!(run.agent, "claude", "the agent is kept by its bare id");
    assert_eq!(run.starter, "ada", "the run is the person's who asked it");
    assert_eq!(run.phase, wire::Phase::Approved);
    assert_eq!(runs.readings().len(), 1);

    let again = runs.ask(ask("Tidy the notes", "claude")).unwrap_err();
    assert!(
        matches!(again, DoorError::InvalidArguments(_)),
        "the second ask is not the first: {again:?}"
    );
}

#[test]
fn an_ask_says_what_it_asks_and_of_whom() {
    let runs = runs(&["claude"]);
    assert!(matches!(
        runs.ask(ask("   ", "claude")).unwrap_err(),
        DoorError::InvalidArguments(_)
    ));
    assert!(matches!(
        runs.ask(ask("Tidy", " ")).unwrap_err(),
        DoorError::InvalidArguments(_)
    ));
    let mut named = ask("Tidy", "claude");
    named.id = Some("../../etc".into());
    assert!(matches!(
        runs.ask(named).unwrap_err(),
        DoorError::InvalidArguments(_)
    ));
    let long = "t".repeat(200);
    runs.ask(ask(&long, "claude")).expect("a long title is cut");
    let run = runs.reading("next").expect("the run");
    assert_eq!(run.title.chars().count(), 120, "a title keeps to a line");
}

#[test]
fn a_session_in_no_room_runs_nothing() {
    let runs = Arc::new(AgentRuns::new(None));
    runs.here("ada", "", Vec::new());
    let refused = runs.ask(ask("Tidy", "claude")).unwrap_err();
    assert!(
        matches!(refused, DoorError::Unavailable { ref code, .. } if code == "no-room"),
        "{refused:?}"
    );
}

#[test]
fn a_message_steers_a_run_until_it_ends() {
    let runs = runs(&["claude"]);
    runs.ask(ask("Tidy", "claude")).unwrap();
    runs.send_message("next", "Also the calendar")
        .expect("the steer");
    let run = runs.reading("next").expect("the run");
    assert_eq!(run.messages, 1);
    assert_eq!(run.directions.len(), 1);
    assert_eq!(run.directions[0].author, "ada");
    assert!(!run.directions[0].incorporated, "its agent has not read it");

    runs.stop("next").expect("the stop");
    assert_eq!(runs.reading("next").unwrap().phase, wire::Phase::Stopped);
    runs.stop("next")
        .expect("stopping it again changes nothing");
    assert!(matches!(
        runs.send_message("next", "Anything").unwrap_err(),
        DoorError::Unavailable { ref code, .. } if code == "run-ended"
    ));
    assert!(matches!(
        runs.record_step("next", program("1 + 1")).unwrap_err(),
        DoorError::Unavailable { .. }
    ));
    assert!(matches!(
        runs.send_message("nope", "Anything").unwrap_err(),
        DoorError::Unavailable { ref code, .. } if code == "no-run"
    ));
}

#[test]
fn what_an_agent_reports_lands_in_its_run() {
    let runs = runs(&["claude"]);
    let asked = runs.ask(ask("Tidy", "claude")).unwrap();
    let issue = asked.issue;
    runs.absorb_report(ROOM, &issue, 1, &runtime_collab::TaskRunUpdate::Accepted);
    assert_eq!(runs.reading("next").unwrap().phase, wire::Phase::Accepted);

    for said in ["one", "two", "three", "four", "five"] {
        runs.absorb_report(
            ROOM,
            &issue,
            1,
            &runtime_collab::TaskRunUpdate::Progress {
                message: said.into(),
                presentation: None,
            },
        );
    }
    let run = runs.reading("next").unwrap();
    assert_eq!(run.phase, wire::Phase::Executing);
    assert_eq!(run.steps, vec!["two", "three", "four", "five"]);
    assert_eq!(run.said.as_deref(), Some("five"));
    assert_eq!(
        run.messages, 0,
        "what its agent says is not a message to it"
    );

    runs.send_message("next", "Also the calendar").unwrap();
    runs.absorb_report(
        ROOM,
        &issue,
        1,
        &runtime_collab::TaskRunUpdate::DirectionsApplied { revision: 1 },
    );
    let run = runs.reading("next").unwrap();
    assert_eq!(run.incorporated, 1, "its agent took the message in");
    assert!(run.directions[0].incorporated);

    runs.absorb_report(
        ROOM,
        &issue,
        1,
        &runtime_collab::TaskRunUpdate::Succeeded {
            message: "tidied".into(),
        },
    );
    let run = runs.reading("next").unwrap();
    assert_eq!(run.phase, wire::Phase::Done);
    assert_eq!(run.said.as_deref(), Some("tidied"));
}

#[test]
fn a_run_that_fails_or_is_stopped_says_why() {
    let runs = runs(&["claude"]);
    let asked = runs.ask(ask("Tidy", "claude")).unwrap();
    runs.absorb_report(
        ROOM,
        &asked.issue,
        1,
        &runtime_collab::TaskRunUpdate::Failed {
            message: "the network is down".into(),
        },
    );
    let run = runs.reading("next").unwrap();
    assert_eq!(run.phase, wire::Phase::Failed);
    assert_eq!(run.said.as_deref(), Some("the network is down"));
    assert_eq!(run.live, None);

    let other = runs
        .ask(Ask {
            id: Some("later".into()),
            ..ask("Summarise", "claude")
        })
        .unwrap();
    runs.absorb_report(
        ROOM,
        &other.issue,
        1,
        &runtime_collab::TaskRunUpdate::Cancelled {
            message: "stopped by the person".into(),
        },
    );
    assert_eq!(
        runs.reading("next").unwrap().phase,
        wire::Phase::Failed,
        "the other run is untouched"
    );
    let stopped = runs.reading("later").unwrap();
    assert_eq!(stopped.phase, wire::Phase::Stopped);
    assert_eq!(stopped.said.as_deref(), Some("stopped by the person"));
}

#[test]
fn a_report_for_another_room_or_epoch_changes_nothing() {
    let runs = runs(&["claude"]);
    let asked = runs.ask(ask("Tidy", "claude")).unwrap();
    runs.absorb_report(
        "another-room",
        &asked.issue,
        1,
        &runtime_collab::TaskRunUpdate::Succeeded {
            message: "elsewhere".into(),
        },
    );
    runs.absorb_report(
        ROOM,
        &asked.issue,
        7,
        &runtime_collab::TaskRunUpdate::Succeeded {
            message: "later".into(),
        },
    );
    runs.absorb_report(
        ROOM,
        "tasks.tracker/issues/nope",
        1,
        &runtime_collab::TaskRunUpdate::Succeeded {
            message: "nobody".into(),
        },
    );
    let run = runs.reading("next").unwrap();
    assert_eq!(run.phase, wire::Phase::Approved);
    assert_eq!(run.said, None);
}

#[test]
fn what_a_run_streams_is_the_newest_snapshot() {
    let runs = runs(&["claude"]);
    let asked = runs.ask(ask("Tidy", "claude")).unwrap();
    let presentation = |sequence: u64, text: &str| runtime_collab::TaskPresentation {
        run: "turn-1".into(),
        sequence,
        received_at: None,
        text: text.into(),
        reasoning: String::new(),
        finish: None,
        detail: String::new(),
    };
    runs.absorb_presentation(ROOM, &asked.issue, 1, Some(&presentation(1, "Writ")));
    assert_eq!(
        runs.reading("next").unwrap().live.unwrap().text,
        "Writ",
        "what the turn says so far"
    );
    runs.absorb_presentation(ROOM, &asked.issue, 1, Some(&presentation(2, "Writing")));
    assert_eq!(runs.reading("next").unwrap().live.unwrap().text, "Writing");
    runs.absorb_presentation(ROOM, &asked.issue, 1, Some(&presentation(1, "older")));
    assert_eq!(
        runs.reading("next").unwrap().live.unwrap().text,
        "Writing",
        "a snapshot the follower has passed changes nothing"
    );
    runs.absorb_presentation(ROOM, &asked.issue, 1, None);
    assert!(runs.reading("next").unwrap().live.is_none());
}

#[test]
fn a_run_reads_on_the_run_channel_as_the_broker_names_it() {
    let runs = runs(&["claude"]);
    let asked = runs.ask(ask("Tidy", "claude")).unwrap();
    runs.send_message("next", "Also the calendar").unwrap();
    let tracked = runs.tracked(ROOM).expect("the room in front's runs");
    assert_eq!(tracked.len(), 1);
    let run = &tracked[0];
    assert_eq!(run.issue, asked.issue);
    assert_eq!(run.agent, "claude");
    assert_eq!(run.starter, "ada");
    assert!(run.active, "a run its agent has not taken up is going");
    assert_eq!(run.directions.len(), 1);
    assert_eq!(run.directions[0].text, "Also the calendar");
    assert_eq!(run.latest_revision(), 1);
    assert_eq!(run.applied_revision(), 0);
    assert!(runs.tracked("another-room").is_err());

    runs.admits(ROOM, "claude")
        .expect("the agent admitted here");
    assert!(runs.admits(ROOM, "someone-else").is_err());
    assert_eq!(runs.room(), ROOM, "the room in front");
}

#[test]
fn a_run_outlives_the_workspace_the_person_moves_to() {
    let runs = runs(&["claude"]);
    runs.ask(ask("Tidy", "claude")).unwrap();
    // The person moves to another workspace: both are open, the other in
    // front. The run asked in the first is still its room's.
    let other = "room-2";
    runs.here(
        "ada",
        other,
        vec![room(ROOM, &["claude"]), room(other, &["scribe"])],
    );
    assert_eq!(runs.room(), other, "the room in front moved");
    let tracked = runs
        .tracked(ROOM)
        .expect("the run's own room is still open");
    assert_eq!(tracked.len(), 1, "its agent still gets it");
    assert_eq!(tracked[0].issue, wire::issue("next"));
    runs.admits(ROOM, "claude").expect("admitted in its room");
    assert!(runs.admits(other, "claude").is_err(), "and nowhere else");
    assert_eq!(
        runs.reading("next").unwrap().phase,
        wire::Phase::Approved,
        "the run goes on"
    );

    // A workspace closed is a room this session cannot speak for.
    runs.here("ada", ROOM, vec![room(ROOM, &["claude"])]);
    assert!(runs.tracked(other).is_err());
    assert!(!runs.knows(other));
}

#[test]
fn a_run_records_the_programs_it_ran_and_the_view_it_answered_with() {
    let runs = runs(&["claude"]);
    runs.ask(ask("Tidy", "claude")).unwrap();
    runs.record_step("next", program("1 + 1"))
        .expect("a program it ran");
    runs.record_answer("next", view())
        .expect("the view it answered with");
    let run = runs.reading("next").unwrap();
    assert_eq!(run.programs.len(), 1);
    assert_eq!(run.programs[0]["program"], "1 + 1");
    let answer = run.answer.expect("an answer");
    assert_eq!(answer["app"], "so.tilo.notes");
    assert_eq!(answer["id"], "n-1", "the view it answered with");
}
