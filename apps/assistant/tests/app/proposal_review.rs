use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use assistant::App;
use serde_json::{Value, json};
use ui::prelude::TestApp;
use ui::tilo_actions::ActionOutput;
use ui::tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, Pending, ReceiptStatus,
    TestProvider, workspace_initiative as initiative,
};

use super::{WORKSPACE, english, says, shows};

/// Each entry is an answer from the initiative host, in order. None of the controls in
/// the test update the answers: a change only becomes visible after a fresh list.
fn initiative_host(replies: Vec<Result<Value, ActionCallError>>) -> Arc<TestProvider> {
    let replies = Mutex::new(VecDeque::from(replies));
    Arc::new(TestProvider::new().answering(move |_, request| {
        let reply = replies.lock().unwrap().pop_front().unwrap_or_else(|| {
            Err(ActionCallError::Refused {
                reason: format!("unexpected action {:?}", request.target),
            })
        });
        Pending::ready(reply.map(|value| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
        }))
    }))
}

fn window(person: &Caller) -> TestApp<App> {
    english();
    let member = person.acting().person_id().expect("the signed-in member");
    let mut app = TestApp::<App>::for_person_in(member, WORKSPACE).expect("the Assistant window");
    app.harness().resize((1280, 1200)).unwrap();
    app.frame();
    app
}

fn wait_for(app: &mut TestApp<App>, control: &str) {
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(2), |frame| {
                frame.targets().contains_key(control)
            }),
        "the window shows {control}"
    );
}

fn click(app: &mut TestApp<App>, control: &str) {
    wait_for(app, control);
    app.harness().click_named(control).unwrap();
    app.settle();
}

fn frame_says(frame: &ui::headless::FrameHarness, text: &str) -> bool {
    frame.a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.description(), node.value()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    })
}

fn wait_for_decided(app: &mut TestApp<App>, gone: &str, status: &str) {
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(2), |frame| {
                !frame.targets().contains_key(gone) && frame_says(frame, status)
            }),
        "{status} in the refreshed list, with {gone} removed"
    );
}

fn assert_asked(host: &TestProvider, person: &Caller, expected: &[(&str, Value)]) {
    let requests = host.asked();
    assert_eq!(requests.len(), expected.len(), "{requests:?}");
    for (request, (action, arguments)) in requests.iter().zip(expected) {
        assert_eq!(
            request.target,
            initiative::target(initiative::PROPOSALS, action)
        );
        let supplied = match &request.argument {
            InvokeArgument::None => json!({}),
            InvokeArgument::Json(object) => Value::Object(object.clone()),
            InvokeArgument::Text(_) => panic!("the initiative contract expects named arguments"),
        };
        assert_eq!(&supplied, arguments);
    }
    assert!(host.callers().iter().all(|caller| caller == person));
}

#[test]
fn opening_proposals_lets_the_person_approve_work_and_reverse_rules_without_pending_work() {
    let task = json!({
        "id": "reply", "kind": "mail", "title": "Draft a reply",
        "why": "The customer is waiting", "task": {"issue": "issue-7", "number": 7},
        "state": "proposed", "proposedAtMs": 12
    });
    let mut accepted = task.clone();
    accepted["state"] = json!("accepted");
    accepted["decidedAtMs"] = json!(18);
    let always = json!({"kind": "mail", "always": true, "quiet": false, "declined": 0});
    let quiet = json!({"kind": "digest", "always": false, "quiet": true, "declined": 2});
    let approved = json!({"proposals": [accepted], "kinds": [always, quiet]});
    let ask_first = json!({"proposals": [accepted], "kinds": [quiet]});
    let resumed = json!({"proposals": [accepted], "kinds": []});
    let host = initiative_host(vec![
        Ok(json!({"proposals": [task]})),
        Ok(accepted),
        Ok(approved),
        Ok(ask_first.clone()),
        Ok(ask_first),
        Ok(resumed.clone()),
        Ok(resumed),
    ]);
    let person = Caller::authenticated([1; 32], [2; 32]).in_workspace(WORKSPACE);
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = window(&person);
    app.call_on("assistant.section:proposals", "open", json!({}))
        .expect("follow Chat's item reference to the published section");
    wait_for(&mut app, "assistant-proposal-always-reply");
    assert!(says(&mut app, "The customer is waiting"));

    click(&mut app, "assistant-proposal-always-reply");
    assert_eq!(host.asked().len(), 1, "choosing always asks for consent");
    click(&mut app, "assistant-proposal-confirm-always-reply");
    wait_for(&mut app, "assistant-kind-ask-first-mail");
    assert!(!shows(&mut app, "assistant-proposal-accept-reply"));
    assert!(
        says(&mut app, "Draft a reply"),
        "the history keeps the task"
    );
    assert!(says(&mut app, "Accepted"));

    click(&mut app, "assistant-kind-ask-first-mail");
    wait_for_decided(&mut app, "assistant-kind-ask-first-mail", "Accepted");
    click(&mut app, "assistant-kind-propose-again-digest");
    wait_for_decided(&mut app, "assistant-kind-propose-again-digest", "Accepted");
    assert_asked(
        &host,
        &person,
        &[
            (initiative::LIST, json!({})),
            (
                initiative::ACCEPT,
                json!({"proposal": "reply", "always": true}),
            ),
            (initiative::LIST, json!({})),
            (initiative::ASK_FIRST, json!({"kind": "mail"})),
            (initiative::LIST, json!({})),
            (initiative::PROPOSE_AGAIN, json!({"kind": "digest"})),
            (initiative::LIST, json!({})),
        ],
    );
}

#[test]
fn the_overview_opens_an_event_for_review_and_keeps_a_refused_decision_visible() {
    let event = json!({
        "id": "weekly", "kind": "schedule", "title": "Schedule the weekly summary",
        "why": "The team reviews progress on Fridays",
        "event": {
            "name": "Weekly shipping summary", "when": "every Friday at 09:00",
            "onlyIf": "Did anything ship?", "instruction": "Summarize releases in Chat"
        },
        "state": "proposed", "proposedAtMs": 21
    });
    let mut declined = event.clone();
    declined["state"] = json!("declined");
    let host = initiative_host(vec![
        Ok(json!({"proposals": [event]})),
        Err(ActionCallError::Refused {
            reason: "Only the workspace member can approve this event".to_owned(),
        }),
        Ok(json!({"proposals": [event]})),
        Ok(declined.clone()),
        Ok(json!({"proposals": [declined]})),
    ]);
    let person = Caller::authenticated([1; 32], [2; 32]).in_workspace(WORKSPACE);
    let _served = ui::RemoteActions::enter(host.clone(), person.clone());
    let mut app = window(&person);
    click(&mut app, "assistant-create");
    click(&mut app, "assistant-review-proposals");
    wait_for(&mut app, "assistant-proposal-accept-weekly");
    for detail in [
        "Weekly shipping summary",
        "every Friday at 09:00",
        "Did anything ship?",
        "Summarize releases in Chat",
    ] {
        assert!(says(&mut app, detail), "the person can review {detail}");
    }
    assert!(!shows(&mut app, "assistant-proposal-always-weekly"));
    let agent = ui::tilo_actions::ActionInvoker::agent("researcher");
    let pressed = app.call_by(
        agent,
        "tilo.control:assistant-proposal-accept-weekly",
        "invoke",
        json!({}),
    );
    app.settle();
    assert!(pressed.is_err() || says(&mut app, "Only you can make this decision"));
    assert_eq!(host.asked().len(), 1, "an agent cannot press as the person");
    click(&mut app, "assistant-proposal-accept-weekly");
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(2), |frame| {
                frame_says(frame, "Only the workspace member can approve this event")
            })
    );
    click(&mut app, "assistant-proposal-why-weekly");
    app.harness().type_text("The team already has a reminder");
    app.frame();
    click(&mut app, "assistant-proposal-decline-weekly");
    wait_for_decided(&mut app, "assistant-proposal-decline-weekly", "Declined");
    assert_asked(
        &host,
        &person,
        &[
            (initiative::LIST, json!({})),
            (initiative::ACCEPT, json!({"proposal": "weekly"})),
            (initiative::LIST, json!({})),
            (
                initiative::DECLINE,
                json!({"proposal": "weekly", "why": "The team already has a reminder"}),
            ),
            (initiative::LIST, json!({})),
        ],
    );
}
