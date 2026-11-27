//! The invocations that hold an engine job: the deadline they are never expired by, and what
//! cancelling them does.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tilo_actions::remote::{ActionCallError, InvocationReceipt, ReceiptStatus};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

use super::{handler_address, handler_arguments, handler_invocation, job_outcome, update_of};
use crate::collab_app_broker::AppBrokerState;

/// This broker's state, with no transport and no workers.
fn broker() -> (crate::collab_broker::AppActionBroker, Arc<AppBrokerState>) {
    let broker = AppBrokerState::test_broker_with_idle_directory_queue();
    let state = Arc::clone(broker.state());
    (broker, state)
}

#[test]
fn an_invocation_that_holds_a_job_is_held_until_its_last_job_ends() {
    let (_broker, state) = broker();
    let invocation = InvocationId::new();
    let key = invocation.to_string();
    assert!(!state.held_by_job(&invocation));
    state.hold_jobs(&key);
    assert!(state.held_by_job(&invocation), "the invocation holds a job");
    state.hold_jobs(&key);
    state.release_jobs(&key);
    assert!(!state.held_by_job(&invocation), "its last job ended");
}

#[test]
fn cancelling_a_held_invocation_cancels_its_jobs_and_answers_cancelled() {
    let (_broker, state) = broker();
    let invocation = InvocationId::new();
    let cancelled = Arc::new(Mutex::new(Vec::new()));
    {
        let cancelled = Arc::clone(&cancelled);
        state.serve_job_canceller(Arc::new(move |invocation: &InvocationId| {
            cancelled.lock().expect("cancellations").push(*invocation);
        }));
    }
    // An invocation that holds no job is cancelled as it always was: the daemon hears nothing.
    assert!(!state.cancel_held_jobs(&invocation));
    assert!(cancelled.lock().expect("cancellations").is_empty());

    state.hold_jobs(&invocation.to_string());
    assert!(state.cancel_held_jobs(&invocation));
    assert_eq!(
        *cancelled.lock().expect("cancellations"),
        vec![invocation],
        "the daemon that runs the job is told"
    );
}

#[test]
fn a_handlers_invocation_is_derived_from_its_parent_and_address() {
    let parent = InvocationId::new();
    let one = handler_invocation(parent, "w", "so.tilo.video", "so.tilo.video", "rendered");
    let again = handler_invocation(parent, "w", "so.tilo.video", "so.tilo.video", "rendered");
    let other = handler_invocation(parent, "w", "so.tilo.video", "so.tilo.video", "progressed");
    assert_eq!(one, again, "a handler dispatched twice is one call");
    assert_ne!(one, other, "two handlers are two calls");
    assert_ne!(one, parent);
    assert_ne!(
        one,
        handler_invocation(
            InvocationId::new(),
            "w",
            "so.tilo.video",
            "so.tilo.video",
            "rendered"
        ),
        "two jobs' handlers are two calls"
    );
}

#[test]
fn a_handler_names_an_action_and_its_arguments() {
    assert_eq!(
        handler_address(&json!({"resource": "so.tilo.video", "action": "rendered"})),
        Some(("so.tilo.video".to_owned(), "rendered".to_owned()))
    );
    assert_eq!(
        handler_address(&json!({"resource": "", "action": "x"})),
        None
    );
    assert_eq!(handler_address(&json!({"resource": "x"})), None);
    assert_eq!(handler_address(&json!(null)), None);
}

#[test]
fn a_handler_is_handed_the_jobs_record_where_the_app_left_it() {
    let record = json!({"id": "1", "state": "done", "detail": {"done": 4, "total": 4}});
    let arguments = handler_arguments(
        &json!({
            "resource": "so.tilo.painter",
            "action": "finished",
            "arguments": {"j": {"$job": null}, "words": "painted"},
        }),
        &record,
    );
    assert_eq!(
        arguments["j"], record,
        "the job's record fills the slot the app left"
    );
    assert_eq!(
        arguments["words"], "painted",
        "the app's own arguments stay"
    );
}

#[test]
fn a_jobs_own_end_is_its_invocations_answer_when_no_handler_ends_it() {
    let invocation = InvocationId::new();
    assert!(matches!(
        job_outcome(invocation, &json!({"state": "done", "detail": {"url": "tilo-live://1"}})),
        InvocationUpdate::Succeeded { output, .. }
            if output.value == Some(json!({"url": "tilo-live://1"}))
    ));
    assert!(matches!(
        job_outcome(invocation, &json!({"state": "failed", "detail": {"why": "out of memory"}})),
        InvocationUpdate::Failed { error, .. } if error.message == "out of memory"
    ));
    assert!(matches!(
        job_outcome(invocation, &json!({"state": "cancelled"})),
        InvocationUpdate::Cancelled { .. }
    ));
}

#[test]
fn a_handlers_receipt_is_its_invocations_answer() {
    let invocation = InvocationId::new();
    assert!(matches!(
        update_of(
            invocation,
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default()
                },
            }
        ),
        InvocationUpdate::Succeeded { .. }
    ));
    assert!(matches!(
        update_of(
            invocation,
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Failed {
                    error: ActionFailure {
                        code: "x".into(),
                        message: "it failed".into(),
                        details: None,
                        retryable: false,
                    },
                },
            }
        ),
        InvocationUpdate::Failed { .. }
    ));
    assert!(matches!(
        update_of(
            invocation,
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Rejected {
                    error: ActionCallError::NotPermitted,
                },
            }
        ),
        InvocationUpdate::Failed { .. }
    ));
    assert!(matches!(
        update_of(
            invocation,
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Cancelled {
                    message: Some("stopped".into())
                },
            }
        ),
        InvocationUpdate::Cancelled { .. }
    ));
    // A handler that has not answered by its deadline ends the invocation as failed, never
    // leaving the caller waiting.
    assert!(matches!(
        update_of(
            invocation,
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            }
        ),
        InvocationUpdate::Failed { .. }
    ));
}
