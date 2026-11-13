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

use std::time::Duration;

use tilo_compositor::InputEvent;
use tilo_session::Session;

use crate::common::{
    CANVAS, CANVAS_CARET, KEY_ESCAPE, click, drag, frame_and_say, key, on_screen, places,
    point_and_say, point_to, private_home, seeded_session, until, wait_for,
};

/// How far an on-screen edge may stand from where the person pointed: rounding through the
/// camera and the caret's own inset, never a misplaced caret.
const NEAR: f64 = 6.0;

/// How far inside a dragged rectangle the caret stands, clear of its outline.
const CARET_STEP: f64 = 12.0;

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

/// Escape lets go of what was typed at the caret with the caret: the next caret starts empty, and
/// what is said there is only what is typed there.
#[test]
fn escape_lets_go_of_the_words_typed_at_the_caret() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let at = crate::common::free_point(&mut session);
    click(&mut session, at);
    caret(&mut session);
    crate::common::type_text(&mut session, "what is on");
    until(&mut session, "the words at the caret", |session| {
        session.shell_drawn_text_bounds("what is on").is_some()
    });
    key(&mut session, KEY_ESCAPE);
    until(&mut session, "Escape takes the caret away", |session| {
        on_screen(session, CANVAS_CARET).is_none()
    });

    click(&mut session, at);
    caret(&mut session);
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        session.shell_drawn_text_bounds("what is on").is_none(),
        "the next caret starts empty"
    );
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
        near(cx, from.0 + CARET_STEP) && near(cy, from.1 + CARET_STEP),
        "the caret stands a step inside the rectangle's top left: {:?} for {from:?}",
        (cx, cy)
    );
    crate::common::say(&mut session, "my mails");

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

/// Every placed strip's canvas box, by the resource its place shows: `(x, y, w, h)` in canvas
/// pixels, as the desk holds it.
fn canvas_box(session: &Session, needle: &str) -> Option<(i64, i64, i64, i64)> {
    places(session).iter().find_map(|place| {
        let strip = crate::common::strip(session, crate::common::number(&place.strip))?;
        (strip.placed && place.record.contains(needle)).then_some((
            strip.canvas_x,
            strip.canvas_y,
            strip.canvas_width,
            strip.canvas_height,
        ))
    })
}

/// Chat holding the person's ask as the turn `question` of `conversation`: what Chat's process
/// does in a person's session, posting the message the ask sent it, answering the send with that
/// message, and holding it in its record, which the session reads the turn back from. Chat's
/// process is not part of this session, so this stands in for that answer and that read;
/// everything around it — the pointing, the send, where the turn stands and what the agent's
/// steps in it fill — is the production path.
fn chat_holds(session: &Session, conversation: &str, question: &str) {
    let turn = format!("chat.workspace.turns:{question}");
    let asked = tilo_session::arrangement::Asked {
        conversation: conversation.to_owned(),
        turn: turn.clone(),
        opened_as: format!(
            "chat.workspace.channels:{}",
            conversation.trim_start_matches("chat.workspace/channels/")
        ),
    };
    // The turn as the session publishes it once Chat holds it: Chat's `turn` view of it.
    let mut published = tilo_actions::ResourceSpec::new(turn, "chat.workspace.turns", "Asked")
        .with_view(tilo_actions::ResourceViewSpec::new("turn", "Asked", "open"));
    published.app_id = Some("chat".to_owned());
    // And the conversation it is in, through Chat's page.
    let mut whole = tilo_actions::ResourceSpec::new(
        asked.opened_as.clone(),
        "chat.workspace.channels",
        "Assistant",
    )
    .with_view(tilo_actions::ResourceViewSpec::new(
        "page",
        "Assistant",
        "open",
    ));
    whole.app_id = Some("chat".to_owned());
    let published = vec![published, whole];
    assert!(
        session.arrangement().borrow().awaits_turns(),
        "the ask waits for Chat to hold it"
    );
    let stood = session.arrangement().borrow_mut().stand_asked(
        &|_| Some((asked.clone(), published.clone())),
        tilo_session::arrangement::Busy::default(),
    );
    assert_eq!(stood, 1, "the ask waited for its turn");
}

/// The conversation the turns of these tests are in, as Chat publishes a channel.
const CONVERSATION: &str = "chat.workspace/channels/c_ask";

/// A question stands its turn at the caret: a place of the turn, through its `turn` view, where
/// the person pointed — not the channel's page, and in no window frame. Until Chat holds the
/// message, nothing stands.
#[test]
fn a_question_stands_its_turn_at_the_caret() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    // What a found thing does where the person clicks: the canvas point the click names.
    let found_at = (x + w * 0.05, y + h * 0.05);
    point_and_say(&mut session, found_at, "my mails");
    let (fx, fy, _, _) = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.lang.mail")
    });
    let at = (x + w * 0.65, y + h * 0.85);

    point_and_say(&mut session, at, "what is on my task list?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|sent| sent.as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    assert!(
        canvas_box(&session, "chat.workspace.turns:").is_none(),
        "nothing stands before Chat holds the message"
    );
    chat_holds(&session, CONVERSATION, "q1");

    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.starts_with("chat.workspace.turns:"))
    });
    assert_eq!(
        turn.shown_as, "turn",
        "the turn stands through its own view"
    );
    assert!(
        !places(&session)
            .iter()
            .any(|place| place.record.contains("channels")),
        "the channel's page is not placed: {:?}",
        places(&session)
    );
    let (tx, ty, tw, th) = canvas_box(&session, "chat.workspace.turns:").expect("the turn's box");
    #[allow(clippy::cast_possible_truncation, reason = "screen pixels")]
    let expected = (
        fx + (at.0 - found_at.0).round() as i64,
        fy + (at.1 - found_at.1).round() as i64,
    );
    assert!(
        (tx - expected.0).abs() <= 2 && (ty - expected.1).abs() <= 2,
        "the turn stands where the person asked: {:?} for {expected:?}",
        (tx, ty)
    );
    assert_eq!(
        (tw, th),
        (520, 240),
        "a clicked turn takes an answer's own measure"
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
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|sent| sent.as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q2");
    let (tx, ty, tw, th) = wait_for(&mut session, "the turn placed", |session| {
        canvas_box(session, "chat.workspace.turns:")
    });
    #[allow(clippy::cast_possible_truncation, reason = "screen pixels")]
    let (rect_w, rect_h) = (
        (to.0 - from.0).round() as i64,
        (to.1 - from.1).round() as i64,
    );
    assert!(
        (tw - rect_w).abs() <= 2,
        "the turn spans the rectangle's width: {tw} for {rect_w}"
    );
    let notes = session
        .resources()
        .into_iter()
        .map(|spec| spec.id.as_str().to_owned())
        .find(|id| id.starts_with("tl:so.tilo.lang.notes:root-"))
        .expect("the notes' root is published");

    // The agent's step in the turn, as the broker invokes it: its `place` names no point, and
    // stands beside the turn's conversation, as the agent door fills it in for a turn's step.
    session
        .invoke_with_actor(
            "agent:assistant",
            "shell:arrangement",
            "place",
            &serde_json::json!({"resource": notes, "beside": CONVERSATION}),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let (nx, ny, nw, nh) = wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.lang.notes")
    });
    assert!(
        nx == tx && nw == tw,
        "the notes fill the rectangle's width: {:?} under the turn's {:?}",
        (nx, nw),
        (tx, tw)
    );
    assert!(
        ny >= ty + th && (ny + nh - (ty + rect_h)).abs() <= 2,
        "the notes fill the rectangle under the turn: {:?} under {:?}, to {}",
        (ny, nh),
        (ty, th),
        ty + rect_h
    );
}

/// The whole conversation a turn opens on request stands on the canvas beside the turn, as Chat's
/// own page: content, not a Chat window, and not in the room the rectangle keeps for the agent's
/// answer. It is opened as a composed view opens anything, through `place` naming only it.
#[test]
fn the_whole_conversation_stands_beside_its_turn() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.05, y + h * 0.05);
    let to = (x + w * 0.45, y + h * 0.95);
    frame_and_say(&mut session, from, to, "what is due?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|sent| sent.as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q3");
    let (tx, ty, tw, th) = wait_for(&mut session, "the turn placed", |session| {
        canvas_box(session, "chat.workspace.turns:")
    });

    // What the turn's "Whole conversation" runs: the system's `place` of the conversation, as
    // the shell places what a composed view opened.
    session
        .invoke(
            "shell:arrangement",
            "place",
            serde_json::json!({"resource": "chat.workspace.channels:c_ask"})
                .as_object()
                .unwrap(),
        )
        .unwrap_or_else(|error| panic!("the conversation is placed: {error:?}"));
    let whole = wait_for(&mut session, "the conversation placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record == "chat.workspace.channels:c_ask")
    });
    assert_eq!(whole.shown_as, "page", "it stands as Chat's page");
    let (cx, cy, _, _) = canvas_box(&session, "chat.workspace.channels:").expect("its box");
    assert!(
        cx >= tx + tw,
        "it stands beside the turn, not in the rectangle's room under it: {:?} beside {:?}",
        (cx, cy),
        (tx, ty, tw, th)
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
    let rest = crate::common::shot(&mut session, "canvas-place-at-rest");
    // The pointer over it: the handle comes over its top trailing corner.
    point_to(&mut session, (px + pw / 2.0, py + ph / 2.0));
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let hovered = crate::common::shot(&mut session, "canvas-place-hovered");
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

/// The right button, which points at a thing.
const BTN_RIGHT: u32 = 0x111;

/// Right-click the screen point `at`.
fn right_click(session: &mut Session, at: (f64, f64)) {
    point_to(session, at);
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_RIGHT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// The inbox, standing in the rectangle from `from` to `to`, and where it stands on screen.
fn inbox_in(
    session: &mut Session,
    from: (f64, f64),
    to: (f64, f64),
) -> (
    tilo_session::arrangement::PlaceReading,
    (f64, f64, f64, f64),
) {
    frame_and_say(session, from, to, "my mails");
    let inbox = wait_for(session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.record.contains("so.tilo.lang.mail"))
    });
    let at = wait_for(session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
            .filter(|(_, _, w, h)| near(*w, to.0 - from.0) && near(*h, to.1 - from.1))
    });
    (inbox, at)
}

/// A right-click on the canvas's free ground points there as a click does: the caret stands at it.
#[test]
fn a_right_click_on_free_space_puts_the_caret_there() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let at = (x + w * 0.45, y + h * 0.55);
    right_click(&mut session, at);
    let (cx, cy, _, _) = caret(&mut session);
    assert!(
        near(cx, at.0) && near(cy, at.1),
        "the caret stands where the right-click pointed: {:?} at {at:?}",
        (cx, cy)
    );
}

/// A right-click on a place's handle opens its resource's own actions as its menu: what the
/// resource publishes, the list its handle's menu and an agent see.
#[test]
fn a_right_click_on_a_place_opens_its_actions() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let (inbox, (px, py, pw, ph)) = inbox_in(
        &mut session,
        (x + w * 0.2, y + h * 0.15),
        (x + w * 0.7, y + h * 0.9),
    );
    let offered: Vec<String> = session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str() == inbox.record)
        .map(|spec| spec.actions.iter().map(|a| a.label.clone()).collect())
        .unwrap_or_default();
    assert!(!offered.is_empty(), "the inbox offers actions: {inbox:?}");
    // The pointer over the place shows its handle; a right-click on its name opens the menu.
    point_to(&mut session, (px + pw * 0.5, py + ph * 0.5));
    let title = if inbox.name.is_empty() {
        inbox.title.clone()
    } else {
        inbox.name.clone()
    };
    let handle = wait_for(&mut session, "the place's handle", |session| {
        session
            .shell_controls(&title)
            .into_iter()
            .filter(|(_, _, w, h)| *w > 0.0 && *h > 0.0)
            .min_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
    });
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    // How many of the resource's actions stand on screen: the menu adds its rows to what the
    // handle's own menu button and the view already show.
    let offered_shown = |session: &Session| -> usize {
        session
            .accessible_labels()
            .iter()
            .filter(|label| offered.contains(label))
            .count()
    };
    let before = offered_shown(&session);
    right_click(
        &mut session,
        (handle.0 + handle.2 / 2.0, handle.1 + handle.3 / 2.0),
    );
    let opened = crate::common::reached(&mut session, |session| offered_shown(session) > before);
    assert!(
        opened,
        "the place's menu lists its actions {offered:?}: {} of them stood before the right-click, \
         {} after; the session says {:?}",
        before,
        offered_shown(&session),
        session.accessible_labels()
    );
}

/// Dragging a place's corner sizes it: its corner follows the pointer and the place keeps that
/// size, one arranging step of the person's.
#[test]
fn dragging_a_places_corner_sizes_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let (inbox, (px, py, pw, ph)) = inbox_in(
        &mut session,
        (x + w * 0.1, y + h * 0.1),
        (x + w * 0.5, y + h * 0.6),
    );
    let corner = (px + pw + 3.0, py + ph + 3.0);
    drag(&mut session, corner, (corner.0 + 120.0, corner.1 + 80.0));
    let resized = crate::common::reached(&mut session, |session| {
        place_on_screen(session, &inbox).is_some_and(|(_, _, nw, _)| !near(nw, pw))
    });
    assert!(
        resized,
        "the inbox resized from {:?} (canvas {:?}, caret {:?}): its strip {:?}, the last step {:?}, refusals {:?}",
        (px, py, pw, ph),
        on_screen(&session, CANVAS),
        on_screen(&session, CANVAS_CARET),
        crate::common::strip(&session, crate::common::number(&inbox.strip)),
        session.arrangement().borrow_mut().last_step(),
        session.shell_refusals()
    );
    let (_, _, nw, nh) = place_on_screen(&session, &inbox).expect("the inbox");
    assert!(
        near(nw, pw + 120.0) && near(nh, ph + 80.0),
        "the place takes the size its corner was dragged to: {:?} from {:?}",
        (nw, nh),
        (pw, ph)
    );
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("a step");
    assert_eq!(
        (who.as_str(), verb.as_str()),
        (crate::common::PERSON, "size")
    );
}

/// The rectangle the person dragged stays outlined while they type what fills it, and goes with
/// the caret.
#[test]
fn the_dragged_rectangle_stays_outlined_while_typing() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.2, y + h * 0.2);
    let to = (x + w * 0.6, y + h * 0.7);
    // The rectangle's bottom edge, half way along: clear of the caret at its top.
    let edge = ((from.0 + to.0) / 2.0, to.1);
    let pixel = |session: &mut Session| -> [u8; 4] {
        let shot = crate::common::shot(session, "canvas-outline");
        let at = ((edge.1.round() as u32 * shot.width + edge.0.round() as u32) * 4) as usize;
        [
            shot.rgba[at],
            shot.rgba[at + 1],
            shot.rgba[at + 2],
            shot.rgba[at + 3],
        ]
    };
    let ground = pixel(&mut session);
    drag(&mut session, from, to);
    caret(&mut session);
    crate::common::type_text(&mut session, "my mails");
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert_ne!(
        pixel(&mut session),
        ground,
        "the rectangle's edge is outlined while the person types"
    );
    key(&mut session, KEY_ESCAPE);
    until(&mut session, "the caret goes", |session| {
        on_screen(session, CANVAS_CARET).is_none()
    });
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert_eq!(
        pixel(&mut session),
        ground,
        "the outline goes with the caret"
    );
}

/// A place's `size` sizes it, its corner where it stood: the arranging action a drag of its edge
/// runs, and what an agent or the REPL runs to size it.
#[test]
fn a_places_size_action_sizes_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let (inbox, _) = inbox_in(
        &mut session,
        (x + w * 0.1, y + h * 0.1),
        (x + w * 0.5, y + h * 0.6),
    );
    session
        .invoke(
            &format!("shell:place:{}", inbox.key),
            "size",
            serde_json::json!({"width": 600, "height": 450})
                .as_object()
                .expect("arguments"),
        )
        .unwrap_or_else(|error| panic!("the size runs: {error:?}"));
    let strip =
        crate::common::strip(&session, crate::common::number(&inbox.strip)).expect("its strip");
    assert_eq!(
        (strip.canvas_width, strip.canvas_height),
        (600, 450),
        "the place takes the size: {strip:?}"
    );
}
