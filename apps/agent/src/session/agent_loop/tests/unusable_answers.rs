//! A model answer the provider failed while writing, or one that arrived and
//! could not be read, ran none of its calls: the loop asks the model again,
//! told why, before the run ends on it. The shape is the one a person's canvas
//! ask stopped on: the model spent its whole output budget without a word
//! (`finish_reason` length, nothing shown), the model proxy ended that stream
//! with an `overloaded_error` event, and the gateway reported the provider
//! failing.

use super::*;
use agent_provider_config::protocol::{ErrorKind, ModelError, StreamEvent};

/// The stream of a provider that failed while writing: some reasoning, then
/// the gateway's failure, then the transport's end.
fn provider_failed_mid_answer() -> Vec<TurnEvent> {
    vec![
        TurnEvent::Neutral(StreamEvent::ReasoningDelta {
            text: "Let me look at the tasks due this week".into(),
        }),
        TurnEvent::Neutral(StreamEvent::Failed {
            error: ModelError::new(ErrorKind::Provider),
        }),
        TurnEvent::Finished(Err(
            "provider stream failed; provider is unavailable; retry later".into(),
        )),
    ]
}

/// The failed turn is asked again, with the reason, and the next answer is the run's.
#[test]
fn an_answer_the_provider_failed_while_writing_is_asked_again_with_the_reason() {
    let mut f = fixture(
        "which of my tasks are due this week?",
        World::default(),
        vec![
            provider_failed_mid_answer(),
            turn(&[finish("Two tasks are due this week.", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        2,
        "the failed answer is asked again, not ended on"
    );
    let asked = bodies[1].to_string();
    assert!(
        asked.contains("did not arrive in a form that could be used")
            && asked.contains("Provider request failed"),
        "the model is told why its last answer was not used: {asked}"
    );
    assert!(
        matches!(
            terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Succeeded { message }
                if message.contains("Two tasks")
        ),
        "the answer asked again completes the task: {:?}",
        terminal(&f.sent.borrow())
    );
}

/// An answer that arrived and could not be read is asked again the same way, with what was
/// wrong with it.
#[test]
fn an_answer_that_could_not_be_read_is_asked_again_with_the_reason() {
    let unreadable = vec![
        TurnEvent::Neutral(StreamEvent::Failed {
            error: ModelError::invalid("provider returned an undeclared tool"),
        }),
        TurnEvent::Finished(Err("invalid model response".into())),
    ];
    let mut f = fixture(
        "my notes",
        World::default(),
        vec![
            unreadable,
            turn(&[finish("Here are your notes.", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    let bodies = f.script.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[1]
            .to_string()
            .contains("provider returned an undeclared tool"),
        "the reason is the model's to read: {}",
        bodies[1]
    );
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded { .. }
    ));
}

/// A provider that keeps failing ends the task after a bounded number of asks, with the
/// person's sentence for it, never a loop.
#[test]
fn a_provider_that_keeps_failing_ends_the_task_after_a_bounded_number_of_asks() {
    let mut f = fixture(
        "which of my tasks are due this week?",
        World::default(),
        (0..6).map(|_| provider_failed_mid_answer()).collect(),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        f.script.bodies.lock().unwrap().len(),
        MOST_UNUSABLE_TURNS + 1,
        "asked again a bounded number of times"
    );
    assert!(
        matches!(
            terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Failed { message }
                if message == "The model's provider failed. Try again."
        ),
        "{:?}",
        terminal(&f.sent.borrow())
    );
}

/// What the person set up is not asked again: a credential the provider refused ends the run
/// on its first answer.
#[test]
fn a_refused_credential_is_not_asked_again() {
    let mut f = fixture(
        "my notes",
        World::default(),
        vec![
            vec![
                TurnEvent::Neutral(StreamEvent::Failed {
                    error: ModelError::new(ErrorKind::Authentication),
                }),
                TurnEvent::Finished(Err("authentication failed".into())),
            ],
            turn(&[finish("never asked", true)], 24),
        ],
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 1);
    assert!(matches!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Failed { .. }
    ));
}

/// One neutral gateway turn calling `calls` under the provider's own ids, as a provider that
/// numbers its calls `call_<n>_<i>` answers.
fn neutral_turn(calls: &[(&str, &str, Value)]) -> Vec<TurnEvent> {
    use agent_provider_config::protocol::{
        Content, Message, ModelResponse, Role, StopReason, Usage,
    };
    let response = ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: calls
                .iter()
                .map(|(id, name, input)| {
                    Content::ToolCall(crate::model::ToolCall {
                        id: (*id).into(),
                        name: (*name).into(),
                        input: input.clone(),
                        extra_content: None,
                    })
                })
                .collect(),
        },
        stop_reason: StopReason::ToolCalls,
        usage: Usage::default(),
        cost_micros: 0,
    };
    vec![
        TurnEvent::Neutral(StreamEvent::Finished {
            response: response.clone(),
        }),
        TurnEvent::Finished(Ok(serde_json::to_string(&response).unwrap())),
    ]
}

/// A provider that reuses a call id an earlier turn of the run used (river's `call_580_0`
/// came back two turns later) has its answer read and run: the call takes an id of its own,
/// and the run finishes on it, never on "the model's answer could not be used".
#[test]
fn a_call_id_the_provider_reuses_from_an_earlier_turn_is_read_not_refused() {
    let (list, nothing) = (tools::LIST_APPLICATIONS, serde_json::json!({}));
    let (plan, reply) = finish("Two tasks are due this week.", true);
    let mut f = fixture(
        "which of my tasks are due this week?",
        World::default(),
        vec![
            neutral_turn(&[("call_580_0", list, nothing)]),
            neutral_turn(&[("call_580_0", plan, reply)]),
        ],
    );
    f.session.chooser = Chooser::scripted(
        Duration::from_secs(5),
        agent_provider_config::ProviderProtocol::OpenAiCompatible,
        Arc::new(Scripted(f.script.clone())),
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(f.script.bodies.lock().unwrap().len(), 2);
    assert!(
        matches!(
            terminal(&f.sent.borrow()),
            runtime_collab::TaskRunUpdate::Succeeded { message }
                if message.contains("Two tasks")
        ),
        "the answer under a reused id is the run's: {:?}",
        terminal(&f.sent.borrow())
    );
}
