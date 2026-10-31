//! The questions the services ask on the portal sheet, answered through
//! its door: every answer names the question it was given for, so one that
//! arrives after the next question opened leaves that one alone.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use futures_util::FutureExt as _;
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_portal::consent::AccessQuestion;
use tilo_session::{Options, Session};
use tilo_sheets::portal::PortalEffect;

const SETTLE: Duration = Duration::from_millis(1);

/// A headless session whose services ask their questions on its sheets.
fn asking() -> (Session, tilo_service::Questions) {
    let questions = tilo_service::Questions::new();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");
    (session, questions)
}

/// The resource publishing `action` among everything the session
/// publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// The question that is on the portal sheet now.
#[track_caller]
fn showing(session: &Session) -> String {
    session
        .portal()
        .expect("portal sheet")
        .dialog()
        .expect("a question is up")
        .handle
}

/// Answer `effect` on the portal sheet's door for `handle`.
#[track_caller]
fn answer(
    session: &mut Session,
    handle: &str,
    effect: &str,
) -> Result<(), tilo_actions::DoorError> {
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    let arguments = serde_json::json!({ "handle": handle })
        .as_object()
        .expect("an object")
        .clone();
    session.invoke(&sheet, effect, &arguments).map(|_| ())
}

#[test]
fn an_answer_for_a_question_that_is_no_longer_up_is_refused() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let ask = |handle: &str, capability: &str| {
        questions.ask(tilo_service::Question {
            handle: handle.to_owned(),
            kind: "portal.access".to_owned(),
            app_id: Some("org.example.Recorder".to_owned()),
            label: "org.example.Recorder".to_owned(),
            details: serde_json::to_value(AccessQuestion {
                capability: capability.to_owned(),
                reason: None,
                may_remember: false,
                text: None,
            })
            .unwrap(),
        })
    };
    let first = ask("portal:/request/access-1", "screencast");
    session.step(SETTLE).expect("step");
    assert_eq!(showing(&session), "portal:/request/access-1");
    answer(
        &mut session,
        "portal:/request/access-1",
        PortalEffect::CONFIRM.id,
    )
    .expect("the question that is up answers");
    let answer_to_first = first.now_or_never().expect("settled").expect("answered");
    assert_eq!(answer_to_first.choice, Some(0), "allowed once");

    // The next question opens before the first one's answer is repeated.
    let second = ask("portal:/request/access-2", "camera");
    session.step(SETTLE).expect("step");
    assert_eq!(showing(&session), "portal:/request/access-2");
    let refused = answer(
        &mut session,
        "portal:/request/access-1",
        PortalEffect::CONFIRM.id,
    )
    .expect_err("an answer for the question that is gone is refused");
    assert!(
        format!("{refused:?}").contains("no longer showing"),
        "the refusal says the question is gone: {refused:?}"
    );
    session.step(SETTLE).expect("step");
    assert_eq!(questions.pending().len(), 1, "the second question waits");
    assert_eq!(showing(&session), "portal:/request/access-2");

    // The question that is up answers itself.
    answer(
        &mut session,
        "portal:/request/access-2",
        PortalEffect::CANCEL.id,
    )
    .expect("decline the question that is up");
    session.step(SETTLE).expect("step");
    assert!(
        second.now_or_never().expect("settled").is_none(),
        "the second was declined, not answered by the first"
    );
}
