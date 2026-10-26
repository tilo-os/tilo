#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use serde_json::json;

use super::{answered, control_of, paused_step};
use crate::services::{TrackerAnswer, TrackerDirection, TrackerQuestion, TrackerRun};

fn direction(id: &str, incorporated: bool) -> TrackerDirection {
    TrackerDirection {
        id: id.into(),
        author: "ada".into(),
        text: format!("message {id}"),
        at_ms: 5_000,
        incorporated,
    }
}

fn run() -> TrackerRun {
    TrackerRun {
        issue: "tasks.tracker/issues/one".into(),
        title: "Tidy the docs".into(),
        instruction: "Tidy the docs".into(),
        agent: "claude".into(),
        epoch: 2,
        starter: "ada".into(),
        active: true,
        budget: None,
        subject: None,
        model: None,
        directions: vec![direction("a", true), direction("b", false)],
        question: None,
    }
}

fn draft(label: &str) -> tilo_actions::Draft {
    serde_json::from_value(json!({
        "action_label": label,
        "resource_label": "Mail",
        "fields": [],
    }))
    .unwrap()
}

fn arguments() -> serde_json::Map<String, serde_json::Value> {
    json!({ "to": "team" }).as_object().unwrap().clone()
}

fn asked(answer: TrackerAnswer, shown: &str) -> TrackerRun {
    let mut run = run();
    run.question = Some(TrackerQuestion {
        id: "q".into(),
        app: "so.tilo.mail".into(),
        resource: "mail.account".into(),
        action: "send".into(),
        arguments_digest: hex::encode(tilo_actions::draft::arguments_digest(&arguments())),
        draft: serde_json::to_value(draft(shown)).unwrap(),
        before: None,
        answer,
    });
    run
}

#[test]
fn steering_is_the_messages_to_the_run_its_agent_has_not_taken_in() {
    let run = run();
    assert_eq!((run.applied_revision(), run.latest_revision()), (1, 2));
    let control = control_of(&run, 1).unwrap();
    assert_eq!(control.from_revision, 1);
    assert_eq!(control.directions.len(), 1);
    assert_eq!(control.directions[0].revision, 2);
    assert_eq!(control.directions[0].text, "message b");
    assert!(!control.has_more);
    assert!(
        control_of(&run, 2).is_err(),
        "a lease cannot claim a message the agent never took in"
    );
}

#[test]
fn a_run_resumes_at_the_question_its_person_has_not_answered() {
    let waiting = asked(TrackerAnswer::Waiting, "Send");
    let paused = paused_step(&waiting).unwrap();
    assert_eq!(
        (paused.app.as_str(), paused.action.as_str()),
        ("so.tilo.mail", "send")
    );
    assert_eq!(
        paused.arguments_digest,
        tilo_actions::draft::arguments_digest(&arguments())
    );
    let confirmed = asked(TrackerAnswer::Confirmed { edits: json!(null) }, "Send");
    assert!(paused_step(&confirmed).is_none());
}

#[test]
fn a_confirmation_admits_only_the_arguments_and_draft_the_person_saw() {
    let target = ("mail.account", "send");
    // Nothing asked about these arguments: the gate asks.
    assert!(
        answered(&run(), target, &arguments(), &draft("Send"))
            .unwrap()
            .is_none()
    );
    // Still waiting.
    let waiting = asked(TrackerAnswer::Waiting, "Send");
    assert!(answered(&waiting, target, &arguments(), &draft("Send")).is_err());
    // Confirmed: the arguments run, under the question's id.
    let confirmed = asked(TrackerAnswer::Confirmed { edits: json!([]) }, "Send");
    let (edited, id) = answered(&confirmed, target, &arguments(), &draft("Send"))
        .unwrap()
        .unwrap();
    assert_eq!((edited, id.as_str()), (arguments(), "q"));
    // A question recorded with another draft than these arguments render
    // admits nothing.
    let misleading = asked(TrackerAnswer::Confirmed { edits: json!([]) }, "Archive");
    assert!(answered(&misleading, target, &arguments(), &draft("Send")).is_err());
    // Other arguments are another question.
    let mut other = arguments();
    other.insert("to".into(), json!("everyone"));
    assert!(
        answered(&confirmed, target, &other, &draft("Send"))
            .unwrap()
            .is_none()
    );
    // Declined: the agent is told why.
    let declined = asked(
        TrackerAnswer::Declined {
            reason: "not today".into(),
        },
        "Send",
    );
    let error = answered(&declined, target, &arguments(), &draft("Send")).unwrap_err();
    assert!(error.contains(tilo_actions::draft::DECLINED) && error.contains("not today"));
}
