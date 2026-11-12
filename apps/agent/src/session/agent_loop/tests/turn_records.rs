//! The turn a run answers, recorded in its conversation: what the reply is
//! told as the run starts, at each step and when it ends.

use super::*;

/// An agent started for a turn reads the catalogue before Chat republished the message it
/// answers: the turn waits for the republication, and its reply is recorded all the same.
#[test]
fn a_turn_waits_for_its_message_to_be_published() {
    let mut f = fixture(
        "write a paragraph",
        World {
            chat: Some("asked".into()),
            chat_late: Some(2),
            ..World::default()
        },
        vec![turn(&[finish("Wrote it.", true)], 24)],
    );
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    assert_eq!(
        records.first().map(|first| first["work"]["state"].clone()),
        Some(Value::from("taken")),
        "the reply was told the run started once the message was published"
    );
    assert_eq!(records.last().unwrap()["say"], "Wrote it.");
}

/// The turn a run answers is recorded in its conversation: the reply is
/// told it was taken at once, of every step, and — once, at the end — what
/// the run said.
#[test]
fn a_run_records_its_turn_in_the_conversation() {
    let mut f = fixture(
        "write a paragraph",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![turn(
            &[
                invoke("append-paragraph", &serde_json::json!({"text": "Rain"})),
                finish("Wrote it.", true),
            ],
            24,
        )],
    );
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    let first = records.first().expect("the reply was told the run started");
    assert_eq!(first["message"], "asked", "on the reply to the message");
    assert_eq!(first["work"]["state"], "taken");
    // How the run was planned is the run's own record, never the reply's.
    for record in &records {
        let work = &record["work"];
        assert!(
            work.get("route").is_none() && work.get("why").is_none(),
            "{work}"
        );
        assert!(
            work.get("did").is_none_or(|did| did == "Append paragraph"),
            "{work}"
        );
    }
    let last = records.last().unwrap();
    assert_eq!(last["work"]["state"], "provider_complete");
    assert_eq!(last["say"], "Wrote it.", "the reply's words, once");
    let steps = last["work"]["trace"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "one step: {steps:?}");
    assert_eq!(steps[0]["label"], "Append paragraph");
    assert_eq!(steps[0]["resource"], "word-processor.document");
    assert_eq!(steps[0]["outcome"], "applied");
}

/// A model or transport error ends the turn with what went wrong, and the conversation is told
/// so: the reply's work ends `refused` carrying the reason, which is the row the page draws
/// with a Retry. Nothing is silent.
///
/// The model is scripted (`Scripted`, the test transport): the run's first turn fails in
/// transport, exactly as a provider that drops the stream does.
#[test]
fn a_run_whose_model_fails_records_the_failure_on_its_reply() {
    let mut f = fixture(
        "write a paragraph",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![vec![TurnEvent::Finished(Err(
            "provider stream reset".into()
        ))]],
    );
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    let last = records.last().expect("the failed turn was recorded");
    assert_eq!(
        last["work"]["state"], "refused",
        "the reply's work says the turn ended without an answer: {last:?}"
    );
    assert!(
        last["say"].as_str().is_some_and(|say| !say.is_empty()),
        "the reply says what happened, in the run's words: {last:?}"
    );
    assert!(
        matches!(
            terminal(&sent),
            runtime_collab::TaskRunUpdate::Failed { .. }
        ),
        "the run tells the host it failed"
    );
}

/// A run the person stops ends its turn: the reply says it was stopped, in
/// the run's words, and the run tells the host it was cancelled.
#[test]
fn a_run_the_person_stops_ends_its_turn() {
    let mut f = fixture(
        "write a paragraph",
        World {
            chat: Some("asked".into()),
            cancel_after_steps: Some(1),
            cancel_delay: Duration::from_millis(100),
            ..World::default()
        },
        vec![
            turn(
                &[invoke(
                    "append-paragraph",
                    &serde_json::json!({"text": "One"}),
                )],
                16,
            ),
            turn(&[finish("Done", true)], 16),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let sent = f.sent.borrow();
    let records = recorded(&sent, "record-work");
    let last = records.last().expect("the turn was recorded");
    assert_eq!(last["work"]["state"], "provider_cancelled");
    assert!(
        last["say"].as_str().is_some_and(|say| !say.is_empty()),
        "the reply says why it stopped: {last:?}"
    );
    assert!(matches!(
        terminal(&sent),
        runtime_collab::TaskRunUpdate::Cancelled { .. }
    ));
}
