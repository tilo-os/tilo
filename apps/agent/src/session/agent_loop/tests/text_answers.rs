//! A model that ends a turn by answering in words, calling nothing: how a
//! chat-style model finishes. Its words are the turn's reply — never an
//! unusable response, never a canned line standing in for what it said — and
//! the turn completes once every step it started has settled.

use super::*;

/// A model whose only turn is text ends the task complete, with its own words
/// as the reply.
#[test]
fn a_text_only_turn_ends_the_task_with_the_models_words() {
    let mut f = fixture(
        "what is on my task list?",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![said_turn(
            "You have three things on your task list: bins, milk, and the boiler.",
        )],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "You have three things on your task list: bins, milk, and the boiler.".into(),
        },
        "the model's own words are the reply"
    );
    // The reply the conversation keeps is its words too, never a canned line.
    let recorded = recorded(&f.sent.borrow(), "record-work");
    let last = recorded.last().expect("the turn was recorded");
    assert_eq!(last["work"]["state"], "provider_complete");
    assert_eq!(
        last["say"],
        serde_json::json!("You have three things on your task list: bins, milk, and the boiler.")
    );
}

/// A model that acts in one turn, then answers in another turn of words, ends
/// the task complete with its own words after the step ran.
#[test]
fn a_text_answer_after_a_tool_call_ends_the_task_with_its_words() {
    let mut f = fixture(
        "write hello",
        World::default(),
        vec![
            turn(
                &[invoke("type-text", &serde_json::json!({"text": "Hello"}))],
                16,
            ),
            said_turn("I typed “Hello” into the document."),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.world.borrow().blocks[0].1, "Hello", "the step ran");
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "I typed “Hello” into the document.".into(),
        },
        "the words after the step are the reply"
    );
}

/// One provider-neutral turn cut off at the model's output limit: the words
/// it wrote before it stopped, and `output_limit` as its stop reason.
fn cut_off_turn(text: &str) -> Vec<TurnEvent> {
    let response = serde_json::json!({
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        "stop_reason": "output_limit"
    });
    vec![
        TurnEvent::Neutral(agent_provider_config::protocol::StreamEvent::TextDelta {
            text: text.into(),
        }),
        TurnEvent::Neutral(agent_provider_config::protocol::StreamEvent::Finished {
            response: serde_json::from_value(response.clone()).unwrap(),
        }),
        TurnEvent::Finished(Ok(response.to_string())),
    ]
}

/// An answer cut off at the model's output limit is continued, not failed:
/// the model writes the first half, stops at its limit, and the loop asks it
/// to go on from where it stopped. The finished turn is the answer the two
/// halves join to, and the run ends complete.
#[test]
fn an_answer_cut_off_at_the_output_limit_is_continued_not_failed() {
    let mut f = fixture(
        "what is on my task list?",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![
            cut_off_turn("You have three things on your task list: "),
            said_turn("bins, milk, and the boiler."),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "the cut-off answer was asked to continue");
    let resumed = bodies[1].to_string();
    assert!(
        resumed.contains("You have three things on your task list: "),
        "the continuation carries what the model already wrote: {resumed}"
    );
    assert!(
        resumed.contains("cut off at the model's output limit"),
        "and says why it is being asked to go on: {resumed}"
    );
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "You have three things on your task list: bins, milk, and the boiler.".into(),
        },
        "the joined answer ends the turn complete, never as a failure"
    );
    let recorded = recorded(&f.sent.borrow(), "record-work");
    let last = recorded.last().expect("the turn was recorded");
    assert_eq!(last["work"]["state"], "provider_complete");
    assert_eq!(
        last["say"],
        serde_json::json!("You have three things on your task list: bins, milk, and the boiler.")
    );
}

/// The output budget is the selected model's own declared limit, never a
/// constant of the loop: a model whose list declares 30000 tokens one answer
/// is asked for exactly that.
#[test]
fn the_requested_output_budget_is_the_models_declared_limit() {
    let mut f = fixture(
        "what is on my task list?",
        World {
            chat: Some("asked".into()),
            ..World::default()
        },
        vec![said_turn("Nothing.")],
    );
    f.session.chooser = crate::model::Chooser::scripted_model(
        Duration::from_secs(5),
        agent_provider_config::ModelDescriptor {
            max_output_tokens: Some(30_000),
            context_window: Some(131_072),
            ..agent_provider_config::ModelDescriptor::new("declares-its-limit")
        },
        Arc::new(Scripted(f.script.clone())),
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.last().expect("the model was asked")["max_output_tokens"],
        serde_json::json!(30_000),
        "the model's own declared output limit is what the agent asks for"
    );
}
