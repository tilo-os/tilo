//! The questions the services ask the person, shown on the session's sheets and answered through
//! the sheet's published actions as the person presses them: an app's access, an administrator's
//! password, a network's secrets, a network that could not be joined, and a device pairing. An
//! allowance given for good is shown at once, and taken back through the portal's own published
//! action.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::common::publisher;
use futures_util::FutureExt as _;
use serde_json::{Map, Value, json};
use tilo_action_policy::Lifetime;
use tilo_actions::{ActionOutput, Actor, ApiError, ResourcePresentation, ResourceSpec};
use tilo_bluetooth_agent::{Method as PairingMethod, PairingDetails};
use tilo_network_secret_agent::questions::{
    CannotJoinDetails, FieldDetails, FieldRule, Refusal, SecretsDetails, SimDetails, SimUnlockStep,
    Text,
};
use tilo_polkit_agent::questions::{AuthenticateDetails, Notice};
use tilo_portal::consent::AccessQuestion;
use tilo_portal::file_chooser::{
    FILE_CHOOSER_QUESTION, FileChooserQuestion, PickerFilter, file_uri,
};
use tilo_portal::screenshot::ScreenshotQuestion;
use tilo_resource::Effect as _;
use tilo_service::{Answer, Asked, Question, Questions, Service, Started, decode, offer};
use tilo_session::{AppSocket, Options, Session};
use tilo_sheets::SheetState as _;
use tilo_sheets::file_picker::{FileMode, FilePickerEffect};
use tilo_sheets::portal::{Body, Dialog, Field, PortalEffect, Prompt, PromptKind};

/// A session whose services ask their questions on its sheets.
fn asking(services: Vec<Box<dyn Service>>) -> (Session, Questions) {
    let questions = Questions::new();
    let session = crate::common::headless(Options {
        questions: Some(questions.clone()),
        services,
        ..Options::default()
    });
    (session, questions)
}

/// Ask `kind` for the app `app` , listed as `label` .
fn ask(questions: &Questions, app: Option<&str>, label: &str, kind: &str, details: Value) -> Asked {
    questions.ask(Question {
        handle: format!("questions-test:/{kind}"),
        kind: kind.to_owned(),
        app_id: app.map(str::to_owned),
        label: label.to_owned(),
        details,
    })
}

/// Let the session run until `done` holds.
#[track_caller]
fn pump(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    tilo_test_env::wait_for("what the test asked for", || {
        session.step(Duration::from_millis(2)).expect("step");
        done(session).then_some(())
    });
}

/// The dialog the portal sheet shows, once the question went up.
#[track_caller]
fn dialog(session: &Session) -> Dialog {
    session
        .portal()
        .expect("portal sheet")
        .dialog()
        .expect("a question is up")
}

/// The secret prompt the portal sheet shows.
#[track_caller]
fn prompt(session: &Session) -> Prompt {
    match dialog(session).body {
        Body::Secret(prompt) => prompt,
        other => panic!("not a secret prompt: {other:?}"),
    }
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// The sheet publishing `effect`, and the arguments a press carries, with the question that is up
/// named when it is one: as the person would press it.
#[track_caller]
fn press_args(session: &Session, effect: &str, args: Value) -> (String, Map<String, Value>) {
    let mut args = arguments(args);
    let handle = session
        .portal()
        .and_then(tilo_sheets::portal::PortalState::dialog)
        .map(|dialog| dialog.handle)
        .or_else(|| {
            session
                .file_picker()
                .and_then(tilo_sheets::file_picker::FilePickerState::request)
                .map(|request| request.handle)
        });
    if let Some(handle) = handle
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    (sheet, args)
}

/// Press `effect` on the sheet publishing it, as the person would.
#[track_caller]
fn press(session: &mut Session, effect: &str, args: Value) {
    let (sheet, args) = press_args(session, effect, args);
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// Press `effect` on the sheet publishing it as `actor` (an agent, a script): what it was
/// answered with.
#[track_caller]
fn press_as(
    session: &mut Session,
    actor: &Actor,
    effect: &str,
    args: Value,
) -> Result<(), ApiError> {
    let (sheet, args) = press_args(session, effect, args);
    let pressed = session.invoke_with_actor(actor, &sheet, effect, &args);
    session.step(Duration::from_millis(1)).expect("step");
    pressed.map(|_| ())
}

/// `pressed` was refused because the question is the asked person's to answer, in words that say
/// so.
#[track_caller]
fn refused_not_the_asked_person(pressed: Result<(), ApiError>, what: &str) {
    match pressed {
        Err(ApiError::Refused { code, reason }) => {
            assert_eq!(code, tilo_actions::NOT_THE_ASKED_PERSON, "{what}: {reason}");
            assert!(
                reason.contains("person"),
                "{what} is refused in words that say whose it is: {reason}"
            );
        }
        Err(other) => panic!("{what} is refused as a question not the agent's: {other:?}"),
        Ok(()) => panic!("{what} was not refused for an agent"),
    }
}

/// The answer to `asked` , once the sheets answered it.
#[track_caller]
fn answered(session: &mut Session, questions: &Questions, asked: Asked) -> Answer {
    pump(session, |_| questions.pending().is_empty());
    asked
        .now_or_never()
        .expect("the question is answered")
        .expect("answered, not declined")
}

/// `asked` was declined, once the sheets did.
#[track_caller]
fn declined(session: &mut Session, questions: &Questions, asked: Asked) {
    pump(session, |_| questions.pending().is_empty());
    assert!(
        asked.now_or_never().expect("the question ended").is_none(),
        "declined"
    );
}

/// What the services hand the sheet as one line of text.
fn words(key: &str, args: &[(&str, Text)]) -> Text {
    Text::Message {
        key: key.to_owned(),
        args: args
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect::<BTreeMap<String, Text>>(),
    }
}

fn access(may_remember: bool, reason: Option<&str>) -> AccessQuestion {
    AccessQuestion {
        capability: "screencast".to_owned(),
        reason: reason.map(str::to_owned),
        may_remember,
        text: None,
    }
}

/// What the portal asks of a screen share the person may allow for good: the question, and the
/// device its answer is kept under.
fn remembered() -> Value {
    let mut details = serde_json::to_value(access(true, None)).expect("details");
    details["device"] = json!("screen");
    details
}

/// A Wi-Fi network asking for its password.
fn wifi(retry: bool) -> Value {
    let mut detail = vec![words(
        "portal-network-enter-password",
        &[("network", Text::Verbatim("Home".to_owned()))],
    )];
    if retry {
        detail.push(words("portal-network-retry", &[]));
    }
    serde_json::to_value(SecretsDetails {
        network: Some("Home".to_owned()),
        title: words("portal-network-wifi-title", &[]),
        detail,
        fields: vec![FieldDetails {
            key: "psk".to_owned(),
            label: words("portal-network-password", &[]),
            masked: true,
            required: true,
            initial: String::new(),
            rule: FieldRule::WpaPassphrase,
        }],
        remember: Some(true),
        sim: None,
        refusal: retry.then_some(Refusal {
            field: 0,
            key: "portal-secret-wifi-length".to_owned(),
        }),
    })
    .expect("details")
}

/// A SIM locked by wrong PINs, asking for its PUK and a new PIN.
fn locked_sim() -> Value {
    serde_json::to_value(SecretsDetails {
        network: Some("Home".to_owned()),
        title: words("portal-sim-puk-title", &[]),
        detail: vec![words(
            "portal-sim-puk-lead",
            &[("network", Text::Verbatim("Home".to_owned()))],
        )],
        fields: vec![
            FieldDetails {
                key: "gsm-puk".to_owned(),
                label: Text::Verbatim("PUK".to_owned()),
                masked: true,
                required: true,
                initial: String::new(),
                rule: FieldRule::Any,
            },
            FieldDetails {
                key: "gsm-pin".to_owned(),
                label: Text::Verbatim("New PIN".to_owned()),
                masked: true,
                required: true,
                initial: String::new(),
                rule: FieldRule::Any,
            },
        ],
        remember: None,
        sim: Some(SimDetails {
            step: SimUnlockStep::Puk,
            attempts_left: Some(2),
        }),
        refusal: None,
    })
    .expect("details")
}

/// A SIM asking for its PIN, and the same after the SIM refused one.
fn sim_pin(retry: bool) -> Value {
    let mut detail = vec![words(
        "portal-network-sim-pin-lead",
        &[("network", Text::Verbatim("Home".to_owned()))],
    )];
    if retry {
        detail.push(words("portal-network-retry", &[]));
    }
    serde_json::to_value(SecretsDetails {
        network: Some("Home".to_owned()),
        title: words("portal-network-sim-pin-title", &[]),
        detail,
        fields: vec![FieldDetails {
            key: "pin".to_owned(),
            label: words("portal-network-sim-pin", &[]),
            masked: true,
            required: true,
            initial: String::new(),
            rule: FieldRule::Any,
        }],
        remember: None,
        sim: Some(SimDetails {
            step: SimUnlockStep::Pin,
            attempts_left: Some(3),
        }),
        refusal: None,
    })
    .expect("details")
}

fn pairing(method: PairingMethod, code: Option<&str>) -> Value {
    serde_json::to_value(PairingDetails {
        device: "Keyboard".to_owned(),
        device_path: "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF".to_owned(),
        method,
        code: code.map(str::to_owned),
        entered: None,
        service: None,
        service_key: None,
    })
    .expect("details")
}

#[test]
fn the_access_question_is_answered_on_the_portal_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.access",
        remembered(),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(
        shown.title, "org.example.Recorder wants to use your screen",
        "what the app asks for, in the person's words"
    );
    assert_eq!(shown.heading, "", "the title already names the app");
    assert_eq!(
        shown.body,
        Body::Choices(vec![
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
            "Always allow".to_owned(),
        ])
    );
    assert_eq!(shown.confirm_label, "Allow");
    assert_eq!(shown.cancel_label, "Deny");

    press(&mut session, PortalEffect::SELECT.id, json!({"index": 2}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(
        answer.choice,
        Some(Lifetime::UntilRevoked.choice()),
        "the third label, Always allow, answers with the lasting lifetime its name says"
    );
    assert!(!answer.remember, "the choice alone says it");

    let grant = dialog(&session);
    assert_eq!(grant.title, "Allowed for good");
    assert_eq!(grant.heading, "", "its detail names the app");
    assert_eq!(
        grant.detail,
        "org.example.Recorder may now use your screen without asking. \
         Take it back to make Tilo ask again next time."
    );
    assert_eq!(grant.confirm_label, "Take back");
    assert_eq!(grant.cancel_label, "Keep");
}

#[test]
fn an_agent_never_answers_a_question_asked_of_the_person() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    // The sheet's answers are published while it is down, so an agent that finds a question
    // waiting reads how to answer it before anything asks one.
    for action in [
        PortalEffect::CONFIRM.id,
        PortalEffect::CANCEL.id,
        PortalEffect::TYPE_SECRET.id,
        PortalEffect::SUBMIT.id,
    ] {
        assert!(
            publisher(&session, action).is_some(),
            "the sheet publishes {action} before a question is up"
        );
    }

    // The access question an agent's own request raised: it may not confirm the allow, nor
    // answer it with "Always", and the question stays up for the person to decide.
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.access",
        remembered(),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    pump(&mut session, |session| {
        session
            .portal()
            .is_some_and(|portal| portal.dialog().is_some())
    });
    let asker = crate::common::agent("org.example.Recorder");
    for (what, effect, args) in [
        ("confirm", PortalEffect::CONFIRM.id, json!({})),
        (
            "allow always",
            PortalEffect::ANSWER.id,
            json!({"index": 2}), // the Always row
        ),
        // Withdrawing its own ask is no step it takes on the sheet either: its host drops the
        // request, which reports the question withdrawn and refuses the step it was for.
        ("cancel", PortalEffect::CANCEL.id, json!({})),
    ] {
        refused_not_the_asked_person(
            press_as(&mut session, &asker, effect, args),
            &format!("the asking agent's {what}"),
        );
        assert_eq!(
            questions.pending().len(),
            1,
            "the question stays up after the asking agent's {what}"
        );
        assert!(
            session.portal().expect("portal sheet").dialog().is_some(),
            "the sheet still shows it after the asking agent's {what}"
        );
    }
    // Another member, who was asked nothing, is refused too: the question is the person's, not
    // any agent's.
    let elsewhere = crate::common::agent("someone-else");
    for (what, effect, args) in [
        ("confirm", PortalEffect::CONFIRM.id, json!({})),
        (
            "allow always",
            PortalEffect::ANSWER.id,
            json!({"index": 2}), // the Always row
        ),
    ] {
        refused_not_the_asked_person(
            press_as(&mut session, &elsewhere, effect, args),
            &format!("another member's {what}"),
        );
    }
    // The person answers the same question through the same published action.
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(
        answer.choice,
        Some(Lifetime::Once.choice()),
        "the answer the sheet showed first was taken"
    );

    // A secret prompt the person's network asked about: what is typed into it and the answer
    // it submits are the person's, so an agent may do neither, its own ask or not.
    let asked = ask(&questions, None, "Home", "network.secrets", wifi(false));
    pump(&mut session, |session| {
        session.portal().is_some_and(|portal| {
            matches!(portal.dialog().map(|dialog| dialog.body), Some(Body::Secret(_)))
        })
    });
    for (what, effect, args) in [
        (
            "typing a secret",
            PortalEffect::TYPE_SECRET.id,
            json!({"field": 0, "value": "short horse"}),
        ),
        ("submitting", PortalEffect::SUBMIT.id, json!({})),
        ("remembering", PortalEffect::REMEMBER.id, json!({})),
    ] {
        for agent in [&asker, &elsewhere] {
            refused_not_the_asked_person(
                press_as(&mut session, agent, effect, args.clone()),
                &format!("an agent's {what}"),
            );
            assert!(
                session
                    .portal()
                    .expect("portal sheet")
                    .dialog()
                    .is_some(),
                "the secret prompt stays up after an agent's {what}"
            );
        }
    }
    let typed_into: Vec<String> = session
        .portal()
        .expect("portal sheet")
        .secrets()
        .iter()
        .map(|buffer| buffer.lock().expect("the prompt's buffer").clone())
        .collect();
    assert!(
        typed_into.iter().all(String::is_empty),
        "nothing an agent typed reached the secret prompt: {typed_into:?}"
    );
    // The person types and submits it.
    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        json!({"field": 0, "value": "short horse"}),
    );
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let typed: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(typed, ["short horse"]);
}

#[test]
fn the_question_of_an_app_that_may_not_remember_offers_no_always() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.access",
        serde_json::to_value(access(false, Some("Look at the barcode"))).expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.detail, "Look at the barcode");
    assert_eq!(
        shown.body,
        Body::Choices(vec![
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
        ])
    );
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    declined(&mut session, &questions, asked);
    assert!(
        session.portal().expect("portal sheet").dialog().is_none(),
        "the sheet is done"
    );
}

#[test]
fn the_screenshot_question_is_taken_from_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.screenshot",
        serde_json::to_value(ScreenshotQuestion {
            parent_window: None,
            modal: true,
        })
        .expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Let org.example.Recorder take a screenshot?");
    assert_eq!(shown.body, Body::Review);
    assert_eq!(shown.confirm_label, "Take screenshot");
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer, Answer::default(), "yes to the picture");
}

#[test]
fn the_administrators_password_is_typed_into_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Authentication is required to change the time",
        "polkit.authenticate",
        serde_json::to_value(AuthenticateDetails {
            message: "Authentication is required to change the time".to_owned(),
            action_id: "org.freedesktop.timedate1.set-time".to_owned(),
            identities: vec!["ada".to_owned()],
            identity: 0,
            prompt: "Password:".to_owned(),
            echo: false,
            notice: Some(Notice::Message {
                key: "portal-auth-rejected".to_owned(),
            }),
        })
        .expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Authentication is required to change the time");
    assert_eq!(shown.heading, "Administrator access");
    assert_eq!(shown.cancel_label, "Cancel");
    let prompt = prompt(&session);
    assert_eq!(
        prompt.resource, "org.freedesktop.timedate1.set-time",
        "what is being authorised, shown to be checked"
    );
    assert_eq!(prompt.fields.len(), 1);
    assert_eq!(prompt.fields[0].label, "Password:");
    assert!(prompt.fields[0].masked);
    assert_eq!(prompt.choices, None, "one identity asks for nothing else");
    assert_eq!(prompt.primary_label, "Authenticate");
    assert_eq!(
        prompt.error,
        Some((0, "That password was not accepted.".to_owned())),
        "why the last attempt failed"
    );

    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        json!({"field": 0, "value": "short horse"}),
    );
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["short horse"]);
    assert_eq!(answer.choice, Some(0), "the identity asked for");
}

#[test]
fn the_wifi_password_is_typed_into_the_sheet_and_remembered() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(&questions, None, "Home", "network.secrets", wifi(false));
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Wi-Fi password required");
    assert_eq!(shown.heading, "");
    assert_eq!(shown.confirm_label, "Connect");
    assert_eq!(shown.cancel_label, "Cancel");
    let asked_for = prompt(&session);
    assert_eq!(asked_for.resource, "Enter the password for “Home”.");
    assert_eq!(asked_for.icon, Some(ui::icons::Icon::Wifi));
    assert_eq!(asked_for.kind, PromptKind::Fields);
    assert_eq!(asked_for.fields.len(), 1);
    assert_eq!(asked_for.fields[0].label, "Password");
    assert!(asked_for.fields[0].masked);
    assert!(asked_for.fields[0].required);
    assert_eq!(
        asked_for.option,
        Some(("Remember on this device".to_owned(), true))
    );
    assert_eq!(asked_for.error, None);

    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        json!({"field": 0, "value": "short horse"}),
    );
    press(&mut session, PortalEffect::REMEMBER.id, json!({}));
    assert_eq!(
        session.portal().expect("portal sheet").option(),
        Some(false),
        "a second press takes the tick back"
    );
    press(&mut session, PortalEffect::REMEMBER.id, json!({}));
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["short horse"]);
    assert!(answer.remember, "kept on this device");
}

#[test]
fn the_secret_the_network_refused_is_asked_again_with_why() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(&questions, None, "Home", "network.secrets", wifi(false));
    pump(&mut session, |_| questions.pending().len() == 1);
    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        json!({"field": 0, "value": "short"}),
    );
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["short"]);

    // The network refuses it and asks again, saying why.
    let _again = ask(&questions, None, "Home", "network.secrets", wifi(true));
    pump(&mut session, |session| {
        session.portal().expect("portal sheet").dialog().is_some()
    });
    assert_eq!(dialog(&session).title, "Wi-Fi password required");
    let refused = prompt(&session);
    assert_eq!(
        refused.error,
        Some((
            0,
            "A Wi-Fi password is 8 to 63 characters, or 64 hexadecimal digits.".to_owned()
        )),
        "why the last secret was not taken"
    );
    assert_eq!(
        refused.resource, "Enter the password for “Home”.",
        "the retry line is not shown as what the network is"
    );
}

#[test]
fn the_sim_locked_by_wrong_pins_asks_for_its_puk_and_a_new_pin() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(&questions, None, "Home", "network.secrets", locked_sim());
    pump(&mut session, |_| questions.pending().len() == 1);
    assert_eq!(dialog(&session).title, "SIM locked");
    let shown = prompt(&session);
    assert_eq!(
        shown.kind,
        PromptKind::SimPuk {
            attempts_left: Some(2)
        }
    );
    assert_eq!(shown.fields.len(), 2, "the PUK and the new PIN");
    assert!(shown.option.is_none(), "the SIM's PIN is not remembered");
    assert_eq!(shown.primary_label, "Unlock");
    assert_eq!(shown.icon, Some(ui::icons::Icon::Lock));
    assert!(shown.resource.contains("Home"));

    // The sheet asks for the new PIN twice; the question reads the PUK and the PIN out of what was
    // typed.
    for (field, value) in [(0, "12345678"), (1, "1234"), (2, "1234")] {
        press(
            &mut session,
            PortalEffect::TYPE_SECRET.id,
            json!({"field": field, "value": value}),
        );
    }
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["12345678", "1234", "1234"]);
}

#[test]
fn a_sim_that_refused_the_pin_asks_for_it_again_with_why() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(&questions, None, "Home", "network.secrets", sim_pin(false));
    pump(&mut session, |_| questions.pending().len() == 1);
    assert_eq!(dialog(&session).title, "SIM PIN required");
    let shown = prompt(&session);
    assert_eq!(
        shown.kind,
        PromptKind::SimPin {
            attempts_left: Some(3)
        }
    );
    assert_eq!(
        shown.fields.len(),
        1,
        "one field to type the PIN into, not one per digit"
    );
    assert!(shown.fields[0].masked, "the PIN is not shown as typed");
    assert_eq!(shown.fields[0].label, "SIM PIN");
    assert!(shown.option.is_none(), "the SIM's PIN is not remembered");
    assert_eq!(shown.primary_label, "Unlock");
    assert_eq!(shown.error, None);
    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        json!({"field": 0, "value": "1234"}),
    );
    press(&mut session, PortalEffect::SUBMIT.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["1234"]);

    // The SIM refuses it and asks again, saying why.
    let _again = ask(&questions, None, "Home", "network.secrets", sim_pin(true));
    pump(&mut session, |session| {
        session
            .portal()
            .expect("portal sheet")
            .dialog()
            .is_some_and(|dialog| match &dialog.body {
                Body::Secret(shown) => shown.error.is_some(),
                _ => false,
            })
    });
    let refused = prompt(&session);
    assert_eq!(refused.error, Some((0, "That PIN is wrong.".to_owned())));
    assert_eq!(
        refused.resource, "Enter the PIN to unlock the SIM for “Home”.",
        "the retry line is not shown as what the SIM is"
    );
}

#[test]
fn the_network_that_could_not_be_joined_offers_to_join_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Cafe",
        "network.cannot-join",
        serde_json::to_value(CannotJoinDetails {
            network: "Cafe".to_owned(),
            title: words(
                "network-cannot-join-title",
                &[("network", Text::Verbatim("Cafe".to_owned()))],
            ),
            body: words("network-cannot-join-body", &[]),
            join: words("network-cannot-join-action", &[]),
        })
        .expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Couldn't join Cafe");
    assert_eq!(shown.heading, "", "the title already names the network");
    assert_eq!(shown.detail, "Enter the password to connect.");
    assert_eq!(shown.body, Body::Choices(vec!["Enter password".to_owned()]));
    assert_eq!(shown.confirm_label, "Enter password");
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.choice, Some(0), "join it as them");
}

#[test]
fn the_pairing_code_both_devices_show_is_confirmed_on_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Keyboard",
        "bluetooth.pair",
        pairing(PairingMethod::ConfirmPasskey, Some("123456")),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Does Keyboard show this code?");
    assert_eq!(shown.code.as_deref(), Some("123456"));
    assert!(
        shown
            .detail
            .starts_with("Compare it with the code on the other device.")
    );
    assert_eq!(shown.confirm_label, "They match");
    assert_eq!(shown.cancel_label, "Don't pair");
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.choice, Some(0), "they match");
    assert!(answer.values.is_empty(), "nothing to type");
}

#[test]
fn the_code_shown_on_the_device_is_typed_into_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Keyboard",
        "bluetooth.pair",
        pairing(PairingMethod::EnterPasskey, None),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Enter the code shown on Keyboard");
    assert_eq!(shown.code, None);
    assert_eq!(
        shown.body,
        Body::Entry {
            masked: false,
            required: true,
            choices: Vec::new(),
        }
    );
    assert_eq!(shown.confirm_label, "Pair");

    // An empty entry is not a pairing with no code.
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    assert_eq!(
        dialog(&session).error.as_deref(),
        Some("Fill in what the question asks first."),
        "the sheet says what is missing"
    );
    assert_eq!(questions.pending().len(), 1, "the question waits");

    press(
        &mut session,
        PortalEffect::TYPE.id,
        json!({"field": "answer", "text": " 042123 "}),
    );
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["042123"], "the code as typed");
    assert_eq!(answer.choice, Some(0));
}

#[test]
fn a_code_to_type_on_the_other_device_offers_nothing_to_confirm() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Keyboard",
        "bluetooth.pair",
        pairing(PairingMethod::DisplayPasskey, Some("654321")),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Type this on Keyboard");
    assert_eq!(shown.code.as_deref(), Some("654321"));
    assert!(
        shown.confirm_label.is_empty(),
        "there is nothing to agree to"
    );
    assert_eq!(shown.cancel_label, "Cancel");

    // Enter on a code on show decides nothing: the pairing goes on until the other device has
    // finished.
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    assert_eq!(questions.pending().len(), 1, "the question waits");
    assert_eq!(
        dialog(&session).code.as_deref(),
        Some("654321"),
        "the code stays up"
    );

    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    declined(&mut session, &questions, asked);
}

#[test]
fn a_typed_pairing_pin_is_masked() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        None,
        "Keyboard",
        "bluetooth.pair",
        pairing(PairingMethod::EnterPin, None),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(shown.title, "Enter the PIN for Keyboard");
    assert_eq!(
        shown.body,
        Body::Entry {
            masked: true,
            required: true,
            choices: Vec::new(),
        }
    );
    press(
        &mut session,
        PortalEffect::TYPE.id,
        json!({"field": "answer", "text": "0000"}),
    );
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["0000"]);
    assert_eq!(answer.choice, Some(0));
    assert_eq!(
        session
            .portal()
            .expect("portal sheet")
            .text(Field::Answer)
            .as_str(),
        "",
        "what was typed does not stay in the sheet"
    );
}

/// What the portal publishes for the allowances it keeps, in memory.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.portal.consent", catalogue = "tests/locales")]
enum Consent {
    /// Forget what the app was allowed
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Forget,
}

fn consent(app: &str, capability: &str) -> String {
    format!("portal:consent:{app}\u{1f}{capability}")
}

/// The portal's side of the allowances, as the portal publishes them.
struct Allowances {
    kept: Arc<Mutex<Vec<(String, String)>>>,
}

impl Service for Allowances {
    fn resources(&self) -> Vec<ResourceSpec> {
        let consents: Vec<ResourceSpec> = {
            let kept = self.kept.lock().expect("kept");
            kept.iter()
                .map(|(app, capability)| {
                    offer::<Consent>(ResourceSpec::new(
                        consent(app, capability),
                        "portal.consent",
                        app.clone(),
                    ))
                    .with_presentation(ResourcePresentation {
                        detail: Some(capability.clone()),
                        ..ResourcePresentation::default()
                    })
                })
                .collect()
        };
        vec![ResourceSpec::new("portal", "portal", "Portal").with_children(consents)]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == "portal")
            .then(|| json!({ "consents": self.kept.lock().expect("kept").clone() }))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let Consent::Forget = decode::<Consent>(&self.resources(), resource, action, arguments)?;
        self.kept
            .lock()
            .expect("kept")
            .retain(|(app, capability)| consent(app, capability) != resource);
        Ok(Started::Done(ActionOutput::default()))
    }
}

/// An app allowed for good is shown its allowance at once, and the sheet's two answers either take
/// it back through the portal's own action or leave it open.
#[test]
fn the_allowance_given_for_good_is_revoked_or_left_access() {
    for revoke in [true, false] {
        let _settled = ui::motion::testing::settled();
        let kept = Arc::new(Mutex::new(vec![(
            "org.example.Recorder".to_owned(),
            "screen".to_owned(),
        )]));
        let (mut session, questions) = asking(vec![Box::new(Allowances {
            kept: Arc::clone(&kept),
        })]);
        let asked = ask(
            &questions,
            Some("org.example.Recorder"),
            "org.example.Recorder",
            "portal.access",
            remembered(),
        );
        pump(&mut session, |_| questions.pending().len() == 1);
        press(&mut session, PortalEffect::SELECT.id, json!({"index": 2}));
        press(&mut session, PortalEffect::CONFIRM.id, json!({}));
        let _answer = answered(&mut session, &questions, asked);
        assert_eq!(
            dialog(&session).confirm_label,
            "Take back",
            "the allowance is shown at once"
        );

        let answer = if revoke {
            PortalEffect::CONFIRM.id
        } else {
            PortalEffect::CANCEL.id
        };
        press(&mut session, answer, json!({}));
        if revoke {
            assert!(
                kept.lock().expect("kept").is_empty(),
                "the portal forgot what it was told to forget"
            );
        } else {
            assert_eq!(
                kept.lock().expect("kept").len(),
                1,
                "keeping it revokes nothing"
            );
        }
        assert!(
            session.portal().expect("portal sheet").dialog().is_none(),
            "the sheet is done"
        );
    }
}

/// A picture editor asking for the name to save its picture under.
fn saving() -> FileChooserQuestion {
    FileChooserQuestion {
        parent_window: None,
        title: "Save the picture".to_owned(),
        save: true,
        accept_label: Some("Export".to_owned()),
        current_name: Some("Untitled.png".to_owned()),
        filters: vec![PickerFilter {
            label: "Images".to_owned(),
            extensions: vec!["png".to_owned()],
            mimetypes: Vec::new(),
        }],
        ..FileChooserQuestion::default()
    }
}

#[test]
fn the_file_question_is_answered_with_what_the_chooser_gave() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        Some("org.example.Editor"),
        "org.example.Editor",
        FILE_CHOOSER_QUESTION,
        serde_json::to_value(saving()).expect("details"),
    );
    let picker = session.file_picker().expect("file chooser").clone();
    pump(&mut session, |_| picker.request().is_some());
    let request = picker.request().expect("the chooser is asked");
    assert_eq!(request.title, "Save the picture");
    assert_eq!(request.mode, FileMode::Save);
    assert_eq!(request.accept_label.as_deref(), Some("Export"));
    assert_eq!(request.suggested_name.as_deref(), Some("Untitled.png"));
    assert_eq!(request.filters.len(), 1, "the app's one type of file");
    assert_eq!(request.filters[0].label, "Images");
    assert_eq!(request.filters[0].extensions, ["png"]);
    assert!(picker.presentation().is_open(), "the chooser sheet is up");

    pump(&mut session, |_| {
        let listing = picker.listing();
        !listing.reading && !listing.folder.as_os_str().is_empty()
    });
    press(
        &mut session,
        FilePickerEffect::SET_FILENAME.id,
        json!({ "name": "Holiday.png" }),
    );
    pump(&mut session, |_| picker.listing().can_confirm);
    press(&mut session, FilePickerEffect::CONFIRM.id, json!({}));

    let answer = answered(&mut session, &questions, asked);
    let written = tilo_paths::home().expect("home").join("Holiday.png");
    assert_eq!(
        answer.uris,
        [file_uri(&written).expect("a file uri")],
        "the app is given the file the person named"
    );
    assert!(picker.request().is_none(), "the chooser is done");
}

#[test]
fn closing_the_chooser_leaves_the_file_question_unanswered() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let asked = ask(
        &questions,
        Some("org.example.Editor"),
        "org.example.Editor",
        FILE_CHOOSER_QUESTION,
        serde_json::to_value(saving()).expect("details"),
    );
    let picker = session.file_picker().expect("file chooser").clone();
    pump(&mut session, |_| picker.request().is_some());

    press(&mut session, FilePickerEffect::CANCEL.id, json!({}));

    declined(&mut session, &questions, asked);
    assert!(picker.request().is_none(), "the chooser is gone");
    assert!(
        !picker.presentation().is_open(),
        "the chooser sheet went away"
    );
}

/// What the agents the shell's workspace holds, as Settings lists them.
fn shell_agents(session: &Session) -> Vec<(String, tilo_action_policy::KindSet)> {
    session.handle().read(|tree| {
        tree.workspaces
            .first()
            .map(|workspace| workspace.agent_members_at(tilo_clock::unix_now()))
            .unwrap_or_default()
    })
}

/// The Agents tab of the workspace in front, as the person opens it: whether it offers what it
/// takes to admit and manage agents, and the agents it lists.
#[track_caller]
fn agents_tab(session: &mut Session) -> (bool, Vec<String>) {
    use tilo_sheets::workspace_settings::{Tab, WorkspaceSettingsEffect};
    let sheet = publisher(session, "workspace-settings")
        .expect("the shell publishes its workspace settings");
    session
        .invoke(&sheet, "workspace-settings", &Map::new())
        .expect("open the workspace settings");
    session.step(Duration::from_millis(1)).expect("step");
    press(
        session,
        WorkspaceSettingsEffect::SHOW_TAB.id,
        json!({"tab": "agents"}),
    );
    let shown = session
        .workspace_settings()
        .expect("workspace settings")
        .settings();
    assert_eq!(shown.tab, Tab::Agents, "the tab the person picked");
    (
        shown.agents.manage,
        shown
            .agents
            .agents
            .into_iter()
            .map(|agent| agent.key)
            .collect(),
    )
}

/// A program of this login at the session's socket: it says who it is, and asks what the session has for it.
/// What it read comes back, however far it got; `hold` keeps the socket until the test
/// lets it go.
fn socket_client(
    socket: &std::path::Path,
    answers: &std::sync::mpsc::Sender<Vec<Value>>,
    hold: Option<std::sync::mpsc::Receiver<()>>,
) {
    let stream = UnixStream::connect(socket).expect("the MCP server's socket");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("the client waits for the person");
    let mut writer = stream.try_clone().expect("a second handle on the socket");
    let mut reader = BufReader::new(stream);
    let mut read = Vec::new();
    for ask in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "2.1"}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "tilo_ls", "arguments": {}}
        }),
    ] {
        if writer.write_all(format!("{ask}\n").as_bytes()).is_err() {
            break;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => read.push(
                serde_json::from_str(&line)
                    .unwrap_or_else(|_| panic!("not a JSON-RPC answer: {line:?}")),
            ),
        }
    }
    let _ = answers.send(read);
    if let Some(hold) = hold {
        let _ = hold.recv_timeout(tilo_test_env::hang_bound());
    }
}

/// The answers the program at the socket read, once the session let it through.
fn socket_answers(
    session: &mut Session,
    questions: &Questions,
    answers: &std::sync::mpsc::Receiver<Vec<Value>>,
) -> Vec<Value> {
    pump(session, |_| questions.pending().is_empty());
    answers
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the program at the socket is served")
}

/// The agents the shell shows at the person's MCP server: the ones whose work the bar carries as a
/// presence of its own.
fn agents_present(session: &Session) -> Vec<String> {
    session
        .read("session:agents")
        .and_then(|agents| agents.get("sessions").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|agent| agent["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn a_program_asking_to_work_as_an_agent_is_admitted_from_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = crate::common::headless(Options {
        questions: Some(questions.clone()),
        collab: false,
        app_socket: AppSocket::At(root.path().join("app_wire.sock")),
        mcp_server: Some(socket.clone()),
        ..Options::default()
    });
    // Nobody set this session's owner up: the shell a device seeds is the person's own, and it is what
    // lets the MCP server's question be answered at all.
    assert!(shell_agents(&session).is_empty(), "nobody works here yet");

    let (answers, received) = std::sync::mpsc::channel();
    let (release, hold) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || socket_client(&socket, &answers, Some(hold)));

    // Nothing the program sent past its introduction is read before the person says so: the
    // question stands on the portal's sheet.
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(
        shown.title, "Work in “Workspace” as the agent Claude Code",
        "where it would work, and as what"
    );
    assert!(
        shown.heading.starts_with("Claude Code (") && shown.heading.ends_with(')'),
        "who asks, and as what: {}",
        shown.heading
    );
    assert_eq!(
        shown.body,
        Body::Choices(vec![
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
            "Always allow".to_owned(),
        ])
    );
    assert_eq!(shown.confirm_label, "Allow");
    assert_eq!(shown.cancel_label, "Deny");
    assert!(
        shell_agents(&session).is_empty(),
        "still nobody, before the answer"
    );

    press(&mut session, PortalEffect::SELECT.id, json!({"index": 1}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let read = socket_answers(&mut session, &questions, &received);

    assert!(
        read[0].pointer("/result/serverInfo").is_some(),
        "the program is served, as an agent: {read:?}"
    );
    assert!(
        read[1].get("error").is_none(),
        "it reaches this desktop's actions as the agent it was admitted as: {read:?}"
    );
    pump(&mut session, |session| {
        agents_present(session)
            .iter()
            .any(|agent| agent == "Claude Code")
    });
    assert_eq!(
        agents_present(&session),
        ["Claude Code".to_owned()],
        "the bar shows the agent the person let in while it works here"
    );
    assert!(
        shell_agents(&session).is_empty(),
        "its membership is the agent member, not a delegation on the shell"
    );
    assert_eq!(
        agents_tab(&mut session),
        (true, vec!["claude-code".to_owned()]),
        "the person administers the shell they sit at, and the client is their agent member there"
    );
    let _ = release.send(());
    client.join().expect("the program's thread");
}

#[test]
fn a_program_the_person_refuses_works_nowhere() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = crate::common::headless(Options {
        questions: Some(questions.clone()),
        collab: false,
        app_socket: AppSocket::At(root.path().join("app_wire.sock")),
        mcp_server: Some(socket.clone()),
        ..Options::default()
    });

    let (answers, received) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || socket_client(&socket, &answers, None));

    pump(&mut session, |_| questions.pending().len() == 1);
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    let read = socket_answers(&mut session, &questions, &received);
    client.join().expect("the program's thread");

    assert_eq!(
        read[0].pointer("/error/message").and_then(Value::as_str),
        Some("You didn’t let this program work here as an agent."),
        "the program reads why: {read:?}"
    );
    assert!(shell_agents(&session).is_empty(), "no agent joined");
}

/// An agent asking its person in its own words, between `choices`, as request `handle`.
fn agent_ask(questions: &Questions, handle: &str, choices: &[&str]) -> Asked {
    let details = serde_json::to_value(tilo_sheets::portal::AgentQuestion {
        agent: "Visa agent".into(),
        workspace: "Home".into(),
        ask: Some(tilo_sheets::portal::AgentAsk {
            question: "Which consulate should I book?".into(),
            choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
            secret: false,
        }),
        ..tilo_sheets::portal::AgentQuestion::default()
    })
    .expect("an agent question");
    questions.ask(Question {
        handle: handle.to_owned(),
        kind: "network.agent-question".to_owned(),
        app_id: None,
        label: "Visa agent".to_owned(),
        details,
    })
}

/// Whether the portal sheet is up, asking request `handle`.
fn asking_now(session: &Session, handle: &str) -> bool {
    session.portal().is_some_and(|portal| {
        portal.presentation().is_open()
            && portal
                .dialog()
                .is_some_and(|dialog| dialog.handle == handle)
    })
}

#[test]
fn ask_me_later_leaves_an_agents_question_open_and_dont_answer_declines_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking(Vec::new());
    let _later = agent_ask(&questions, "agent-ask:later", &["Berlin", "Munich"]);
    pump(&mut session, |session| {
        asking_now(session, "agent-ask:later")
    });
    let shown = dialog(&session);
    assert_eq!(shown.defer_label.as_deref(), Some("Ask me later"));
    assert_eq!(shown.cancel_label, "Don't answer");

    press(
        &mut session,
        PortalEffect::Defer {
            handle: String::new(),
        }
        .id(),
        json!({}),
    );
    pump(&mut session, |session| {
        !session
            .portal()
            .expect("portal sheet")
            .presentation()
            .is_open()
    });
    for _ in 0..20 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert_eq!(
        questions.pending().len(),
        1,
        "the question put off stays open"
    );
    assert!(
        !asking_now(&session, "agent-ask:later"),
        "a question put off is not asked again until the person chooses it"
    );

    // Another question comes up, and declining it declines it alone.
    let declining = agent_ask(&questions, "agent-ask:no", &["Yes", "No"]);
    pump(&mut session, |session| asking_now(session, "agent-ask:no"));
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    pump(&mut session, |_| questions.pending().len() == 1);
    assert!(
        declining
            .now_or_never()
            .expect("the question ended")
            .is_none(),
        "declined"
    );
    assert_eq!(questions.pending()[0].handle, "agent-ask:later");
}
