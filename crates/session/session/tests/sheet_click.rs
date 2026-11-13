//! A press with the pointer on a trusted sheet's own button answers with that button, in a real
//! session: the sheet the session raises over the whole display, landed on where the card's footer
//! draws its actions.
//!
//! The bug this file was written for: on the system trust sheet ("Let the agent Edit a text
//! field"), a click on "Allow once" at its drawn and reported place was taken as a decline. The
//! sheet's layout was right; what the press hit was not the button. This test drives the
//! production path a person does — the session's own portal sheet, its published controls saying
//! where the button stands, and a compositor-level pointer press there — and reads the answer the
//! question got.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use futures_util::FutureExt as _;
use serde_json::json;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::{Asked, Question, Questions};
use tilo_session::{Options, Session};

/// The kind of question a conversation turn's ask is.
const TURN_QUESTION: &str = "session.turn-question";

/// A headless session at a 1440 by 900 display, as the run that showed the bug had.
fn session_at(width: i32, height: i32) -> (Session, Questions) {
    session_scaled(width, height, 1.0)
}

/// [`session_at`] on a display drawing at `scale`: physical pixels per logical pixel, as a
/// high-density panel does.
fn session_scaled(width: i32, height: i32, scale: f64) -> (Session, Questions) {
    let questions = Questions::new();
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height).with_scale(scale)],
        Options {
            questions: Some(questions.clone()),
            // The shell as the whole screen, as the development session runs it.
            shell: true,
            collab: false,
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(16)).expect("a frame");
    (session, questions)
}

/// Ask a conversation turn's trust question, as the session's own turn does.
fn ask_a_turn(questions: &Questions) -> Asked {
    questions.ask(Question {
        handle: "sheet-click:/turn-question".into(),
        kind: TURN_QUESTION.to_owned(),
        app_id: None,
        label: "Edit a text field".into(),
        details: json!({
            "resource_label": "Groceries",
            "action_label": "Edit a text field",
            "draft": {
                "action_label": "Edit a text field",
                "resource_label": "Groceries",
                "fields": [{
                    "path": ["title"],
                    "label": "Title",
                    "value": {"type": "text", "text": "Groceries"},
                    "editable": true,
                    "multiline": false,
                    "shows": null,
                    "role": null
                }]
            }
        }),
    })
}

/// Where the session shows the control named `name`, in desk coordinates.
#[track_caller]
fn where_shown(session: &Session, name: &str) -> (f64, f64, f64, f64) {
    session
        .controls()
        .into_iter()
        .find(|control| control.name == name)
        .map(|control| control.rect)
        .unwrap_or_else(|| {
            let names: Vec<String> = session
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect();
            panic!("no control named {name:?}; the session shows {names:?}")
        })
}

/// Press and release the left button at `(x, y)` on the session's display, as the person does.
fn click(session: &mut Session, (x, y): (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    session.step(Duration::from_millis(1)).expect("step");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Let the session settle: frames until the sheet's card has stopped arriving.
fn settled(session: &mut Session) {
    for _ in 0..8 {
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// A click on the button "Allow once" at the place the sheet publishes it answers the question
/// with the allow.
#[test]
fn a_click_on_allow_once_at_its_reported_place_allows_the_step() {
    // Motion reads at its end: this test checks the frame it lands on.
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = session_at(1440, 900);
    let mut asked = ask_a_turn(&questions);
    crate::common::wait_for(&mut session, "the ask's sheet", |session| {
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .map(|_| ())
    });
    settled(&mut session);
    let (x, y, w, h) = where_shown(&session, "Allow once");
    assert!(
        w > 0.0 && h > 0.0,
        "the button stands somewhere: {x},{y} {w}x{h}"
    );
    assert!(
        x >= 0.0 && y >= 0.0 && x + w <= 1440.0 && y + h <= 900.0,
        "the button stands on the display: {x},{y} {w}x{h}"
    );
    click(&mut session, (x + w / 2.0, y + h / 2.0));
    let answer = crate::common::wait_for(&mut session, "the ask's answer", |_| {
        (&mut asked).now_or_never()
    });
    let answer = answer.expect("the step is allowed, not declined");
    assert_eq!(answer.choice, None, "allowing once carries no other choice");
}

/// A press that arrives before any pointer motion — the pointer was never reported over the
/// sheet — must not answer the ask: nothing was pressed on the sheet.
#[test]
fn a_press_with_no_pointer_over_the_sheet_answers_nothing() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = session_at(1440, 900);
    let mut asked = ask_a_turn(&questions);
    crate::common::wait_for(&mut session, "the ask's sheet", |session| {
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .map(|_| ())
    });
    settled(&mut session);
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(
        (&mut asked).now_or_never().is_none(),
        "a press that never reached the sheet answered it"
    );
}

/// A press beside the card — on the scrim, away from every button — answers nothing. A
/// confirmation an agent is waiting on is not declined by a stray press: declining is Escape or
/// the button that says so, so that a press meant for nothing is never recorded as a refusal.
#[test]
fn a_press_beside_the_card_does_not_decline_the_step() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = session_at(1440, 900);
    let mut asked = ask_a_turn(&questions);
    crate::common::wait_for(&mut session, "the ask's sheet", |session| {
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .map(|_| ())
    });
    settled(&mut session);
    // Well clear of the card: the scrim, near the display's top-left corner.
    click(&mut session, (40.0, 40.0));
    settled(&mut session);
    assert!(
        (&mut asked).now_or_never().is_none(),
        "a press beside the sheet answered the ask"
    );
    assert!(
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .is_some(),
        "the ask is still up on its sheet"
    );
}

/// The same, on a display drawing at 2 pixels per logical pixel: the pointer the compositor
/// delivers is in logical desks pixels, and the sheet's card is laid out in the same ones, so a
/// press on the button answers with it at any scale.
#[test]
fn a_click_on_allow_once_answers_at_a_scaled_display() {
    let _settled = ui::motion::testing::settled();
    // A 1440x900 display drawn at 2: the same logical desk, twice the pixels.
    let (mut session, questions) = session_scaled(2880, 1800, 2.0);
    let mut asked = ask_a_turn(&questions);
    crate::common::wait_for(&mut session, "the ask's sheet", |session| {
        session
            .portal()
            .and_then(tilo_sheets::portal::PortalSeat::dialog)
            .map(|_| ())
    });
    settled(&mut session);
    let (x, y, w, h) = where_shown(&session, "Allow once");
    assert!(
        x >= 0.0 && y >= 0.0 && x + w <= 1440.0 && y + h <= 900.0,
        "the button stands on the logical display at scale 2: {x},{y} {w}x{h}"
    );
    click(&mut session, (x + w / 2.0, y + h / 2.0));
    let answer = crate::common::wait_for(&mut session, "the ask's answer", |_| {
        (&mut asked).now_or_never()
    });
    assert!(
        answer.is_some(),
        "the step is allowed at scale 2, not declined"
    );
}
