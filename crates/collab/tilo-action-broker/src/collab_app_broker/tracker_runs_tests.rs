#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use serde_json::json;

use super::answered;
use crate::services::{TrackerAnswer, TrackerQuestion, TrackerRun};

fn run() -> TrackerRun {
    TrackerRun {
        issue: "chat.workspace/channels/loop/messages/one".into(),
        title: "Tidy the docs".into(),
        instruction: "Tidy the docs".into(),
        agent: "claude".into(),
        epoch: 2,
        starter: "ada".into(),
        active: true,
        finished_by_agent: false,
        subject: None,
        turn: None,
        question: None,
        delegation: None,
        origin: None,
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

tilo_test_env::scenario! {
    fn a_tracker_run_admits_only_what_its_person_saw;

    phase a_confirmation_admits_only_the_arguments_and_draft_the_person_saw;
    {
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
}
