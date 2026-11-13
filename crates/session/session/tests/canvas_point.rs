//! Pointing on the canvas and saying what goes there (`docs/designs/app-language/canvas.md`).
//!
//! The canvas has no fixed bar: a click on free ground puts a caret at the point, a dragged
//! rectangle puts it inside the rectangle, Escape takes it away, and Enter sends what was typed
//! there through the shell's own `place-found`. What a find names fills the rectangle; a question
//! stands its turn where it was asked, never the chat's channel page, and the places the agent's
//! steps in that turn make fill the rectangle. A place is its view on the canvas, with no frame:
//! its handle shows only while the pointer is over it.
//!
//! Every step is the production path: the person's pointer and keys through the compositor, the
//! shell's own views and actions, the session's arrangement. What is stubbed: the agent's model;
//! its step is invoked as the agent, exactly as the broker invokes it for the turn.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::time::Duration;

use tilo_compositor::InputEvent;
use tilo_session::Session;

use common::{
    CANVAS, CANVAS_CARET, KEY_ESCAPE, click, drag, frame_and_say, key, on_screen, places,
    point_and_say, point_to, private_home, seeded_session, until, wait_for,
};

/// How far an on-screen edge may stand from where the person pointed: rounding through the
/// camera and the caret's own inset, never a misplaced caret.
const NEAR: f64 = 6.0;

/// The middle button, which pans the canvas.
const BTN_MIDDLE: u32 = 0x112;
/// The left Ctrl key, held for a wheel that zooms.
const KEY_CTRL: u32 = 29;

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= NEAR
}

/// Where the place `place` stands on screen: the group its view stands in, named for what it shows
/// (the widest box named so — its handle's label is named the same).
fn place_on_screen(
    session: &Session,
    place: &tilo_session::arrangement::PlaceReading,
) -> Option<(f64, f64, f64, f64)> {
    let title = if place.name.is_empty() {
        &place.title
    } else {
        &place.name
    };
    session
        .shell_controls(title)
        .into_iter()
        .filter(|(_, _, w, h)| *w > 0.0 && *h > 0.0)
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// The caret's field on screen, once it stands.
fn caret(session: &mut Session) -> (f64, f64, f64, f64) {
    wait_for(session, "the caret", |session| {
        on_screen(session, CANVAS_CARET)
    })
}

/// Pan the canvas by a middle drag of `by` and zoom it in two wheel detents with Ctrl held, about
/// its middle: a lens that is neither the origin nor the canvas's own scale.
fn pan_and_zoom(session: &mut Session, by: (f64, f64)) {
    let (x, y, w, h) = wait_for(session, "the canvas", |session| on_screen(session, CANVAS));
    let middle = (x + w / 2.0, y + h / 2.0);
    point_to(session, middle);
    for pressed in [true, false] {
        if pressed {
            session
                .compositor()
                .input(InputEvent::PointerButton {
                    button: BTN_MIDDLE,
                    pressed: true,
                })
                .unwrap();
            session.step(Duration::from_millis(1)).unwrap();
            for step in 1..=6 {
                let t = f64::from(step) / 6.0;
                point_to(session, (middle.0 + by.0 * t, middle.1 + by.1 * t));
            }
        } else {
            session
                .compositor()
                .input(InputEvent::PointerButton {
                    button: BTN_MIDDLE,
                    pressed: false,
                })
                .unwrap();
            session.step(Duration::from_millis(1)).unwrap();
        }
    }
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: KEY_CTRL,
            pressed: true,
        })
        .unwrap();
    for _ in 0..2 {
        session
            .compositor()
            .input(InputEvent::PointerAxis {
                horizontal: 0.0,
                vertical: -15.0,
            })
            .unwrap();
        session.step(Duration::from_millis(16)).unwrap();
    }
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: KEY_CTRL,
            pressed: false,
        })
        .unwrap();
    session.step(Duration::from_millis(16)).unwrap();
}

/// The canvas has no fixed bar: before the person points, nothing to type in stands on it — not
/// the old "Find something, or ask" field, not a caret.
#[test]
fn the_canvas_has_no_fixed_bar() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        on_screen(&session, "Find something, or ask").is_none(),
        "no fixed bar stands on the canvas: {:?}",
        session.accessible_labels()
    );
    assert!(
        on_screen(&session, CANVAS_CARET).is_none(),
        "no caret stands before the person points"
    );
}

/// A click on free ground puts the caret at the point, under a lens that is panned and zoomed;
/// Escape takes it away, and a click elsewhere puts it there.
#[test]
fn a_click_puts_the_caret_where_it_points_under_pan_and_zoom() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    pan_and_zoom(&mut session, (-140.0, -90.0));
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");

    for at in [(x + w * 0.3, y + h * 0.35), (x + w * 0.62, y + h * 0.7)] {
        click(&mut session, at);
        let (cx, cy, _, _) = caret(&mut session);
        assert!(
            near(cx, at.0) && near(cy, at.1),
            "the caret stands where the person pointed: {:?} at {at:?}",
            (cx, cy)
        );
        key(&mut session, KEY_ESCAPE);
        until(&mut session, "Escape takes the caret away", |session| {
            on_screen(session, CANVAS_CARET).is_none()
        });
    }
}

/// A rectangle dragged on free ground and "my mails" said in it: the inbox fills the rectangle.
#[test]
fn a_dragged_rectangle_is_filled_by_what_is_found() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.2, y + h * 0.2);
    let to = (x + w * 0.75, y + h * 0.85);

    drag(&mut session, from, to);
    let (cx, cy, _, _) = caret(&mut session);
    assert!(
        near(cx, from.0) && near(cy, from.1),
        "the caret stands at the rectangle's top left: {:?} for {from:?}",
        (cx, cy)
    );
    common::say(&mut session, "my mails");

    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.contains("so.tilo.lang.mail"))
    });
    let (px, py, pw, ph) = wait_for(&mut session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
    });
    assert!(
        near(px, from.0) && near(py, from.1),
        "the inbox stands at the rectangle's corner: {:?} for {from:?}; {inbox:?} named {:?}",
        (px, py, pw, ph),
        session.shell_controls(&inbox.name)
    );
    assert!(
        near(pw, to.0 - from.0) && near(ph, to.1 - from.1),
        "the inbox fills the rectangle: {:?} for {:?}",
        (pw, ph),
        (to.0 - from.0, to.1 - from.1)
    );
}

/// A question stands its turn at the caret: a place of the turn, through its `turn` view, where
/// the person pointed — not the channel's page, and in no window frame.
#[test]
fn a_question_stands_its_turn_at_the_caret() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let at = (x + w * 0.4, y + h * 0.3);

    point_and_say(&mut session, at, "what is on my task list?");

    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.starts_with("chat.workspace/turns/"))
    });
    assert_eq!(
        turn.shown_as, "turn",
        "the turn stands through its own view"
    );
    assert!(
        !places(&session)
            .iter()
            .any(|place| place.record.contains("/channels/")),
        "the channel's page is not placed: {:?}",
        places(&session)
    );
    let (px, py, _, _) = wait_for(&mut session, "the turn on screen", |session| {
        place_on_screen(session, &turn)
    });
    assert!(
        near(px, at.0) && near(py, at.1),
        "the turn stands where the person asked: {:?} at {at:?}",
        (px, py)
    );
    until(&mut session, "the person's words in the turn", |session| {
        session
            .shell_drawn_text_bounds("what is on my task list?")
            .is_some()
    });
    assert!(
        session
            .shell_drawn_text_bounds("what is on my task list?")
            .is_some_and(|(_, ty, _, _)| ty >= py),
        "the person's line stands in the turn, not in a title bar over it"
    );
}

/// The agent's `place` in a turn asked in a dragged rectangle fills the rest of the rectangle
/// under the turn: the rectangle is the turn's anchor.
#[test]
fn an_agents_place_in_the_turn_fills_the_dragged_rectangle() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.1, y + h * 0.1);
    let to = (x + w * 0.8, y + h * 0.95);

    frame_and_say(&mut session, from, to, "what do my notes say?");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.starts_with("chat.workspace/turns/"))
    });
    let (_, ty, _, th) = wait_for(&mut session, "the turn on screen", |session| {
        place_on_screen(session, &turn)
    });
    let conversation = session
        .service_resources()
        .into_iter()
        .map(|spec| spec.id.as_str().to_owned())
        .find(|id| id.starts_with("chat.workspace/channels/"))
        .expect("the conversation the ask made is published");
    let notes = session
        .service_resources()
        .into_iter()
        .map(|spec| spec.id.as_str().to_owned())
        .find(|id| id.starts_with("tl:so.tilo.lang.notes:root-"))
        .expect("the notes' root is published");

    // The agent's step in the turn, as the broker invokes it: its `place` names no point, and the
    // session fills in the turn's conversation as what it stands beside.
    session
        .invoke_with_actor(
            "agent:assistant",
            "shell:arrangement",
            "place",
            &serde_json::json!({"resource": notes, "beside": conversation}),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let shown = wait_for(&mut session, "the notes placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record == notes)
    });
    let (nx, ny, nw, nh) = wait_for(&mut session, "the notes on screen", |session| {
        place_on_screen(session, &shown)
    });
    assert!(
        near(nx, from.0) && near(nw, to.0 - from.0),
        "the notes fill the rectangle's width: {:?} for {from:?}..{to:?}",
        (nx, nw)
    );
    assert!(
        ny >= ty + th && near(ny + nh, to.1),
        "the notes fill the rectangle under the turn: {:?} under {:?}, to {}",
        (ny, nh),
        (ty, th),
        to.1
    );
}

/// A place is its view on the canvas with no frame: its handle (its name, pin and close) is not
/// drawn until the pointer is over it, and then it is.
#[test]
fn a_place_has_no_frame_and_its_handle_shows_on_hover() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.15, y + h * 0.2);
    let to = (x + w * 0.6, y + h * 0.8);
    frame_and_say(&mut session, from, to, "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.contains("so.tilo.lang.mail"))
    });
    let (px, py, pw, ph) = wait_for(&mut session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
    });
    // The pointer away from it: nothing of a handle is drawn over the place's top edge.
    point_to(&mut session, (x + w * 0.9, y + h * 0.9));
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let rest = common::shot(&mut session, "canvas-place-at-rest");
    // The pointer over it: the handle comes over its top trailing corner.
    point_to(&mut session, (px + pw / 2.0, py + ph / 2.0));
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let hovered = common::shot(&mut session, "canvas-place-hovered");
    let band = |shot: &tilo_compositor::Screenshot| -> Vec<u8> {
        let mut out = Vec::new();
        let top = (py - 24.0).max(0.0) as u32;
        let left = (px + pw * 0.5) as u32;
        let right = (px + pw) as u32;
        for row in top..(py as u32 + 24) {
            for col in left..right {
                let at = ((row * shot.width + col) * 4) as usize;
                out.extend_from_slice(&shot.rgba[at..at + 4]);
            }
        }
        out
    };
    let (before, after) = (band(&rest), band(&hovered));
    let changed = before
        .chunks(4)
        .zip(after.chunks(4))
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        changed > 200,
        "hovering the place shows its handle over its top trailing corner ({changed} pixels changed)"
    );
    assert!(
        session.shell_control("Close").is_some(),
        "the handle holds the place's close"
    );
}
