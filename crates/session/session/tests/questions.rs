//! The questions the services ask the person, shown on the session's
//! sheets and answered through the sheet's published actions as the person
//! presses them: an app's access, an administrator's password, a network's
//! secrets, a network that could not be joined, and a device pairing. An
//! allowance given for good is shown at once, and taken back through the
//! portal's own published action.

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

use futures_util::FutureExt as _;
use serde_json::{Map, Value, json};
use tilo_actions::{ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_bluetooth_agent::{Method as PairingMethod, PairingDetails};
use tilo_compositor::OutputConfig;
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
use tilo_service::{Answer, Asked, Question, Questions, Service, Started, decode, offer};
use tilo_session::{ActionPlane, Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::{FileMode, FilePickerEffect};
use tilo_sheets::portal::{Body, Dialog, Field, PortalEffect, Prompt, PromptKind};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;

/// A session whose services ask their questions on its sheets.
fn asking(services: Vec<Box<dyn Service>>) -> (Session, Questions) {
    let questions = Questions::new();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            services,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, questions)
}

/// Ask `kind` for the app `app`, listed as `label`.
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
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
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

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// Press `effect` on the sheet publishing it, as the person would.
#[track_caller]
fn press(session: &mut Session, effect: &str, args: Value) {
    let mut args = arguments(args);
    let handle = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .map(|dialog| dialog.handle)
        .or_else(|| {
            session
                .file_picker()
                .and_then(tilo_sheets::file_picker::FilePickerSeat::request)
                .map(|request| request.handle)
        });
    if let Some(handle) = handle
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// The answer to `asked`, once the sheets answered it.
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
        serde_json::to_value(access(true, None)).expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(
        shown.title, "org.example.Recorder wants to use your screen",
        "what the app asks for, in the person's words"
    );
    assert_eq!(shown.heading, "org.example.Recorder");
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
    assert_eq!(answer.choice, Some(2), "allowed for good");
    assert!(!answer.remember, "the choice alone says it");

    let grant = dialog(&session);
    assert_eq!(grant.title, "Allowed for good");
    assert_eq!(grant.heading, "org.example.Recorder");
    assert_eq!(
        grant.detail,
        "org.example.Recorder may now use your screen without asking. \
         Revoke it to make Tilo ask again next time."
    );
    assert_eq!(grant.confirm_label, "Revoke");
    assert_eq!(grant.cancel_label, "Keep");
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

    // The sheet asks for the new PIN twice; the question reads the PUK and
    // the PIN out of what was typed.
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
    assert_eq!(shown.heading, "Cafe");
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

    // Enter on a code on show decides nothing: the pairing goes on until
    // the other device has finished.
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
#[effect(operation = "so.tilo.portal.consent")]
enum Consent {
    /// Forget what the app was allowed
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
    ) -> Result<Started, DoorError> {
        let Consent::Forget = decode::<Consent>(&self.resources(), resource, action, arguments)?;
        self.kept
            .lock()
            .expect("kept")
            .retain(|(app, capability)| consent(app, capability) != resource);
        Ok(Started::Done(ActionOutput::default()))
    }
}

#[test]
fn the_allowance_given_for_good_is_taken_back_through_the_ports_action() {
    let _settled = ui::motion::testing::settled();
    let kept = Arc::new(Mutex::new(vec![(
        "org.example.Recorder".to_owned(),
        "screencast".to_owned(),
    )]));
    let (mut session, questions) = asking(vec![Box::new(Allowances {
        kept: Arc::clone(&kept),
    })]);
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.access",
        serde_json::to_value(access(true, None)).expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    press(&mut session, PortalEffect::SELECT.id, json!({"index": 2}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let _answer = answered(&mut session, &questions, asked);
    assert_eq!(
        dialog(&session).confirm_label,
        "Revoke",
        "the allowance is shown at once"
    );

    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    assert!(
        kept.lock().expect("kept").is_empty(),
        "the portal forgot what it was told to forget"
    );
    assert!(
        session.portal().expect("portal sheet").dialog().is_none(),
        "the sheet is done"
    );
}

#[test]
fn keeping_the_allowance_leaves_it_standing() {
    let _settled = ui::motion::testing::settled();
    let kept = Arc::new(Mutex::new(vec![(
        "org.example.Recorder".to_owned(),
        "screencast".to_owned(),
    )]));
    let (mut session, questions) = asking(vec![Box::new(Allowances {
        kept: Arc::clone(&kept),
    })]);
    let asked = ask(
        &questions,
        Some("org.example.Recorder"),
        "org.example.Recorder",
        "portal.access",
        serde_json::to_value(access(true, None)).expect("details"),
    );
    pump(&mut session, |_| questions.pending().len() == 1);
    press(&mut session, PortalEffect::SELECT.id, json!({"index": 2}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let _answer = answered(&mut session, &questions, asked);
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    assert_eq!(
        kept.lock().expect("kept").len(),
        1,
        "keeping it revokes nothing"
    );
    assert!(
        session.portal().expect("portal sheet").dialog().is_none(),
        "the sheet is done"
    );
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

/// A directory only this user reaches: the broker refuses to serve a socket
/// anybody else could open, and the agent door is this account's alone.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// The workspace of the desk `session` shows, owned by the person at it: a
/// workspace nobody administers admits nobody.
fn own_the_desk(session: &Session) {
    let mut tree = session.handle().read(Clone::clone);
    for workspace in &mut tree.workspaces {
        workspace.set_owner("person");
    }
    session.handle().replace_tree(tree);
}

/// What the agents the desk's workspace holds, as Settings lists them.
fn desk_agents(session: &Session) -> Vec<(String, tilo_action_policy::KindSet)> {
    session.handle().read(|tree| {
        tree.workspaces
            .first()
            .map(|workspace| workspace.agent_members_at(tilo_clock::unix_now()))
            .unwrap_or_default()
    })
}

/// A program of this login at the door: it says who it is, and asks what the
/// session has for it. What it read comes back, however far it got.
fn door_client(socket: &std::path::Path, answers: &std::sync::mpsc::Sender<Vec<Value>>) {
    let stream = UnixStream::connect(socket).expect("the agent door's socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("the client waits for the person");
    let mut writer = stream.try_clone().expect("a second handle on the door");
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
}

/// The answers the program at the door read, once the session let it
/// through.
fn door_answers(
    session: &mut Session,
    questions: &Questions,
    answers: &std::sync::mpsc::Receiver<Vec<Value>>,
    client: std::thread::JoinHandle<()>,
) -> Vec<Value> {
    pump(session, |_| questions.pending().is_empty());
    let read = answers
        .recv_timeout(Duration::from_secs(30))
        .expect("the program at the door is served");
    client.join().expect("the program's thread");
    read
}

#[test]
fn a_program_asking_to_work_as_an_agent_is_admitted_from_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    own_the_desk(&session);
    assert!(desk_agents(&session).is_empty(), "nobody works here yet");

    let (answers, received) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || door_client(&socket, &answers));

    // Nothing the program sent past its introduction is read before the
    // person says so: the question stands on the portal's sheet.
    pump(&mut session, |_| questions.pending().len() == 1);
    let shown = dialog(&session);
    assert_eq!(
        shown.title, "Work in “Desk” as the agent claude-code",
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
        desk_agents(&session).is_empty(),
        "still nobody, before the answer"
    );

    press(&mut session, PortalEffect::SELECT.id, json!({"index": 1}));
    press(&mut session, PortalEffect::CONFIRM.id, json!({}));
    let read = door_answers(&mut session, &questions, &received, client);

    assert!(
        read[0].pointer("/result/serverInfo").is_some(),
        "the program is served, as an agent: {read:?}"
    );
    assert!(
        read[1].get("error").is_none(),
        "it reaches this desktop's actions as the agent it was admitted as: {read:?}"
    );
    assert_eq!(
        desk_agents(&session),
        [("claude-code".to_owned(), tilo_action_policy::KindSet::READ)],
        "the agent the workspace holds, with the Observer preset's grants"
    );
}

#[test]
fn a_program_the_person_refuses_works_nowhere() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("agent.sock");
    let questions = Questions::new();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            action_plane: ActionPlane::At(root.path().join("plane.sock")),
            agent_door: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    own_the_desk(&session);

    let (answers, received) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || door_client(&socket, &answers));

    pump(&mut session, |_| questions.pending().len() == 1);
    press(&mut session, PortalEffect::CANCEL.id, json!({}));
    let read = door_answers(&mut session, &questions, &received, client);

    assert_eq!(
        read[0].pointer("/error/message").and_then(Value::as_str),
        Some("You didn’t let this program work here as an agent."),
        "the program reads why: {read:?}"
    );
    assert!(desk_agents(&session).is_empty(), "no agent joined");
}
