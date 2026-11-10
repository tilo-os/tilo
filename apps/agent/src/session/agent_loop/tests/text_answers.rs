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
