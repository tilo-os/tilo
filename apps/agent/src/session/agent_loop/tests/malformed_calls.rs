//! A model whose own tool call is malformed — `invoke` with no `operation`,
//! or one carrying an argument its declaration does not name — is answered
//! with what was wrong as a tool error result and given another turn to
//! correct, rather than ending the run. A model that keeps calling the same
//! way ends the task after a bounded number of those turns, so the person
//! hears what it could not do instead of the run looping.

use super::*;

/// `invoke` without the `operation` argument its declaration requires, as a
/// provider's turn carrying it: the shape a person's task stopped on.
fn invoke_missing_operation() -> (&'static str, Value) {
    (tools::INVOKE, serde_json::json!({"parameters": {}}))
}

/// The malformed turn is answered with an error result naming what was wrong
/// and what the call needs, and the corrected call in the next turn runs.
#[test]
fn a_malformed_tool_call_is_answered_with_what_was_wrong_and_the_turn_taken_again() {
    let mut f = fixture(
        "what is on my task list?",
        World::default(),
        vec![
            turn(&[invoke_missing_operation()], 24),
            turn(&[finish("Three things, listed in your Tasks.", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();

    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        2,
        "the malformed turn is answered, not failed"
    );
    // The second request carries the refused turn and the tool's answer to it,
    // marked as an error, naming the missing argument.
    let messages = bodies[1]["messages"].as_array().unwrap();
    let result = messages
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_result")
        .unwrap_or_else(|| panic!("the refused call gets a tool result: {messages:?}"));
    assert_eq!(
        result["is_error"],
        serde_json::json!(true),
        "the answer is an error result, not a note to the person"
    );
    let answer = result["content"].as_str().unwrap();
    assert!(
        answer.contains("missing operation"),
        "it says what was wrong: {answer}"
    );
    assert!(
        answer.contains("required arguments are: operation"),
        "and which arguments the call needs: {answer}"
    );
    assert!(
        matches!(
            terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Succeeded { message }
                if message.contains("Three things")
        ),
        "the corrected turn completes the task: {:?}",
        terminal(&f.sent.borrow())
    );
}

/// A turn where one call is malformed does not run the other calls' actions:
/// the whole batch is refused and answered, so nothing half of a turn runs.
#[test]
fn a_turn_with_a_malformed_call_runs_none_of_its_calls() {
    let mut f = fixture(
        "write hello",
        World::default(),
        vec![
            turn(
                &[
                    invoke("type-text", &serde_json::json!({"text": "Do not write"})),
                    invoke_missing_operation(),
                ],
                8,
            ),
            turn(&[finish("Nothing was written; ask again.", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert!(
        f.world.borrow().steps.is_empty(),
        "a refused batch dispatches nothing"
    );
    assert!(
        !f.sent
            .borrow()
            .iter()
            .any(|request| matches!(request, CollabRequest::InvokeTaskAction { .. })),
        "no action was asked of the application"
    );
    assert_eq!(
        f.script.bodies.lock().unwrap().len(),
        2,
        "the turn is answered and taken again"
    );
}

/// A model that keeps calling the same way ends the task after a bounded
/// number of corrective turns, rather than looping: the person hears what it
/// could not do.
#[test]
fn a_model_that_keeps_malforming_its_call_ends_the_task_without_looping() {
    let malformed = || turn(&[invoke_missing_operation()], 24);
    let mut f = fixture(
        "what is on my task list?",
        World::default(),
        vec![
            malformed(),
            malformed(),
            malformed(),
            malformed(),
            malformed(),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let turns = f.script.bodies.lock().unwrap().len();
    assert!(
        (2..=3).contains(&turns),
        "a bounded number of turns, not {turns}"
    );
    assert!(
        matches!(
            terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Failed { message }
                if message.contains("missing operation")
        ),
        "the run ends and the conversation shows why: {:?}",
        terminal(&f.sent.borrow())
    );
}

/// A call naming a tool the loop does not offer is answered the same way:
/// the model reads that there is no such tool and may call another.
#[test]
fn a_call_to_a_tool_the_loop_does_not_offer_is_answered_not_ended() {
    let mut f = fixture(
        "what is on my task list?",
        World::default(),
        vec![
            turn(&[("unoffered", serde_json::json!({}))], 24),
            turn(&[finish("Answered.", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let answer = bodies[1].to_string();
    assert!(
        answer.contains("There is no tool called unoffered"),
        "the model is told the tool is not offered: {answer}"
    );
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));
}
