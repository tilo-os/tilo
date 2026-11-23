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
//! shell's own views and actions, the session's layout. What is stubbed: the agent's model;
//! its step is invoked as the agent, exactly as the broker invokes it for the turn.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_compositor::InputEvent;
use tilo_session::Session;
use tilo_session_core::surface::Scroll;

use crate::common::{
    CANVAS, CANVAS_CARET, KEY_ESCAPE, PERSON, click, drag, frame_and_say, key, on_screen, places,
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
    place: &tilo_session_core::layout::PlaceReading,
) -> Option<(f64, f64, f64, f64)> {
    // A place on the canvas is named on its handle by its own title, a window by its name.
    [&place.title, &place.name]
        .into_iter()
        .filter(|name| !name.is_empty())
        .flat_map(|name| session.shell_controls(name))
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
    let (left, top, width, height) =
        wait_for(session, "the canvas", |session| on_screen(session, CANVAS));
    let middle = (left + width / 2.0, top + height / 2.0);
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
            .input(InputEvent::PointerAxis(Scroll::Notches(0.0, -1.0)))
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

    // Each point leaves the zoomed caret's line room to its right: one that would not fit makes
    // the camera pan to keep it whole, and the caret then stands left of the point.
    for at in [(x + w * 0.3, y + h * 0.35), (x + w * 0.5, y + h * 0.7)] {
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

/// The first ask after the person switches to the framed canvas is not lost: a click and the words
/// typed straight after it, with no pause for the caret, stand at the caret and are sent.
#[test]
fn the_first_ask_after_switching_to_frames_is_typed_and_sent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = crate::common::Api::open(&mut session, root.path());
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, style: \"so.tilo.shell:strips\")",
    );
    assert!(!failed, "the strips: {answer}");
    until(&mut session, "the strips", |session| {
        on_screen(session, CANVAS).is_none()
    });
    let (answer, failed) = repl.run(
        &mut session,
        "shell.show_style(shell.viewing, style: \"so.tilo.shell:frames\")",
    );
    assert!(!failed, "the frames: {answer}");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    // At once, as a person who switched and clicks to ask does: click, then type.
    let at = crate::common::free_point(&mut session);
    click(&mut session, at);
    crate::common::type_text(&mut session, "what is on my list");
    until(&mut session, "the words at the caret", |session| {
        session
            .shell_drawn_text_bounds("what is on my list")
            .is_some()
    });
    key(&mut session, crate::common::KEY_ENTER);
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
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

/// Drag a rectangle on free ground and say `text` in it: what it finds, by `needle` in its record,
/// fills the rectangle on screen — its surface at the rectangle's corner and its size, the
/// handle's band over it.
fn the_rectangle_is_filled(text: &str, needle: &str) {
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
    crate::common::say(&mut session, text);

    let found = wait_for(&mut session, "what it finds placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains(needle))
    });
    let (px, py, pw, ph) = wait_for(
        &mut session,
        format!("{needle} filling the rectangle {from:?}..{to:?} on screen"),
        |session| {
            place_on_screen(session, &found)
                .filter(|(_, _, pw, ph)| near(*pw, to.0 - from.0) && near(*ph, to.1 - from.1))
        },
    );
    assert!(
        near(px, from.0) && near(py, from.1),
        "its surface stands at the rectangle's corner: {:?} for {from:?}; {found:?}",
        (px, py, pw, ph),
    );
}

/// A rectangle dragged on free ground and "my mails" said in it: the inbox fills the rectangle.
#[test]
fn a_dragged_rectangle_is_filled_by_what_is_found() {
    the_rectangle_is_filled("my mails", "so.tilo.mail");
}

/// The notes found in a rectangle fill it, as the inbox does: the place is the rectangle's
/// height, never the height its view would hug.
#[test]
fn a_dragged_rectangle_is_filled_by_the_notes() {
    the_rectangle_is_filled("my notes", "so.tilo.notes");
}

/// The first point down the column at `x` from `top` (screen logical pixels, a screen `width`
/// wide) that is the canvas's own ground: where the card drawn at `top` ends. `None` while
/// nothing under `top` is ground again.
fn card_foot(session: &mut Session, (x, top): (f64, f64), width: f64) -> Option<f64> {
    let snap = crate::common::shot(session, "card-foot");
    let scale = f64::from(snap.width) / width.max(1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "screen pixels"
    )]
    let at = |y: f64| -> Option<[u8; 3]> {
        let (px, py) = ((x * scale) as u32, (y * scale) as u32);
        (py < snap.height).then(|| {
            let i = ((py * snap.width + px) * 4) as usize;
            [snap.rgba[i], snap.rgba[i + 1], snap.rgba[i + 2]]
        })
    };
    let ground = at(top - 30.0)?;
    let mut y = top + 1.0;
    while let Some(px) = at(y) {
        let below = at(y + 4.0);
        if px == ground && below == Some(ground) {
            return Some(y);
        }
        y += 1.0;
    }
    None
}

/// A rectangle dragged just under an inbox that hugs its few rows, on ground that is free on the
/// screen though the inbox's place could grow into it: what is found there fills the rectangle,
/// at its corner, never below the height the inbox could grow to.
#[test]
fn a_rectangle_under_a_hugging_place_is_filled_where_it_was_dragged() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.1, y + h * 0.08), "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
    });
    let (ix, iy, _, _) = wait_for(&mut session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
    });
    // Where the inbox's card ends on screen: the first point under it, down a column through
    // it, that is the canvas's own ground again.
    // Its rows arrive after the card first draws: the foot is where the card settles, the same
    // twice a few frames apart.
    let mut last = None;
    let foot = wait_for(&mut session, "the inbox's card settled", |session| {
        let now = card_foot(session, (ix + 30.0, iy), w)?;
        for _ in 0..20 {
            session.step(std::time::Duration::from_millis(16)).ok()?;
        }
        let settled = (last == Some(now)).then_some(now);
        last = Some(now);
        settled
    });
    let from = (ix, foot + 60.0);
    let to = (ix + w * 0.4, (from.1 + 260.0).min(y + h - 20.0));
    frame_and_say(&mut session, from, to, "my notes");
    let notes = wait_for(&mut session, "the notes placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.notes"))
    });
    let (px, py, _, _) = wait_for(&mut session, "the notes on screen", |session| {
        place_on_screen(session, &notes)
    });
    assert!(
        near(px, from.0) && near(py, from.1),
        "the notes stand at the rectangle's corner {from:?}, not below the height the inbox \
         could grow to: {:?} (its card drawn to {foot})",
        (px, py)
    );
}

/// A click and what is found there: its surface's top-left stands at the point clicked, the
/// handle's band over it.
#[test]
fn what_is_found_at_a_click_stands_at_the_point() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let at = (x + w * 0.1, y + h * 0.2);
    point_and_say(&mut session, at, "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
    });
    let (px, py, _, _) = wait_for(&mut session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
    });
    assert!(
        near(px, at.0) && near(py, at.1),
        "its surface's top-left is where the person clicked: {:?} for {at:?}",
        (px, py)
    );
    // A click names no size: the place is as tall as what it shows, up to a window's height, and
    // never stands taller than that most.
    let strip =
        crate::common::strip(&session, crate::common::number(&inbox.strip)).expect("its strip");
    assert!(
        strip.canvas_hugs,
        "a thing found at a click hugs what it shows: {strip:?}"
    );
    let (_, _, _, ph) = place_on_screen(&session, &inbox).expect("its surface");
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    let most = strip.canvas_height as f64;
    assert!(
        ph <= most + 0.5,
        "it stands no taller than its most: {ph} of {most}"
    );
}

/// A click left of a place that stands already places what is found at the click (`canvas.md`
/// §1): "my notes" said left of the inbox stands with its top-left where the person clicked,
/// narrowed to the free area before the inbox, never below or beside the inbox instead.
#[test]
fn what_is_found_left_of_a_place_stands_at_the_click_narrowed() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.45, y + h * 0.1), "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    let at = (x + w * 0.05, y + h * 0.3);
    point_and_say(&mut session, at, "my notes");
    let notes = wait_for(&mut session, "the notes placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.notes"))
    });
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let (px, py, _, _) = wait_for(&mut session, "the notes on screen", |session| {
        place_on_screen(session, &notes)
    });
    assert!(
        near(px, at.0) && near(py, at.1),
        "the notes' top-left is where the person clicked: {:?} for {at:?}",
        (px, py)
    );
    let (nx, _, nw, _) = canvas_box(&session, "so.tilo.notes").expect("the notes' box");
    let gap = i64::from(tilo_desktop_model::CANVAS_GAP);
    assert!(
        nw < 720,
        "narrowed from its own width to the room before the inbox: {nw}"
    );
    assert_eq!(
        nx + nw + gap,
        inbox.0,
        "as wide as the free room up to the inbox, a gap clear of it: notes at {nx} wide {nw}, inbox {inbox:?}"
    );
    assert!(
        nw >= i64::from(tilo_desktop_model::WINDOW_MIN_WIDTH),
        "never under a window's least width: {nw}"
    );
}

/// What is found at a click is named for what it is and hugs what it shows (`canvas.md` §7): "my
/// mails" stands captioned as the inbox's own view is called, never by the account or the first
/// record its app's root goes by, and "my notes" as the notes; each list stands as tall as its
/// rows, not at a window's height with an empty foot; and the one under the other keeps its
/// caption clear of it.
#[test]
fn what_is_found_at_a_click_is_named_for_what_it_is_and_hugs_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.05, y + h * 0.1), "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
    });
    assert_eq!(inbox.title, "All inboxes", "named for its view: {inbox:?}");
    point_and_say(&mut session, (x + w * 0.65, y + h * 0.1), "my notes");
    let notes = wait_for(&mut session, "the notes placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.notes"))
    });
    assert_eq!(notes.title, "Notes", "named for its view: {notes:?}");
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    for place in [&inbox, &notes] {
        until(
            &mut session,
            &format!("{} captioned on screen", place.title),
            |session| !session.shell_controls(&place.title).is_empty(),
        );
        let strip =
            crate::common::strip(&session, crate::common::number(&place.strip)).expect("its strip");
        let (_, _, _, ph) = wait_for(&mut session, "it on screen", |session| {
            place_on_screen(session, place)
        });
        #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
        let most = strip.canvas_height as f64;
        assert!(
            ph < most * 0.9,
            "{} hugs its rows rather than at its most: {ph} of {most}",
            place.title
        );
    }
}

/// Every placed strip's canvas box, by the resource its place shows: `(x, y, w, h)` in canvas
/// pixels, as the shell holds it.
fn canvas_box(session: &Session, needle: &str) -> Option<(i64, i64, i64, i64)> {
    places(session).iter().find_map(|place| {
        let strip = crate::common::strip(session, crate::common::number(&place.strip))?;
        (strip.placed && place.resource.contains(needle)).then_some((
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
    let asked = tilo_session_core::layout::Asked {
        conversation: conversation.to_owned(),
        message: question.to_owned(),
        turn: turn.clone(),
        opened_as: format!(
            "chat.workspace.channels:{}",
            conversation.trim_start_matches("chat.workspace/channels/")
        ),
    };
    // The turn as the session publishes it once Chat holds it: Chat's `turn` view of it, named
    // for its question, so two turns on the canvas are told apart on screen.
    let mut published = tilo_actions::ResourceSpec::new(turn, "chat.workspace.turns", question)
        .with_view(tilo_actions::ResourceViewSpec::new(
            "turn", question, "open",
        ));
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
        session.layout().borrow().awaits_turns(),
        "the ask waits for Chat to hold it"
    );
    let stood = session.layout().borrow_mut().stand_asked(
        &|_| Some((asked.clone(), published.clone())),
        tilo_session_core::layout::Busy::default(),
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
    // What a found thing does where the person clicks: the canvas point the click names, low
    // enough that the caption over it stands in view, so the canvas has no reason to pan.
    let found_at = (x + w * 0.05, y + h * 0.1);
    point_and_say(&mut session, found_at, "my mails");
    let (fx, fy, _, _) = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    let at = (x + w * 0.65, y + h * 0.85);

    point_and_say(&mut session, at, "what is on my task list?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
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
            .find(|place| place.resource.starts_with("chat.workspace.turns:"))
    });
    assert_eq!(
        turn.shown_as, "turn",
        "the turn stands through its own view"
    );
    assert!(
        !places(&session)
            .iter()
            .any(|place| place.resource.contains("channels")),
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
    // An answer's own measure, as far as the room to the edge of what the person sees allows (the
    // canvas is at zoom 1, so a screen pixel is a canvas pixel).
    #[allow(clippy::cast_possible_truncation, reason = "screen pixels")]
    let to_edge = (x + w - at.0).round() as i64 - 24;
    assert_eq!(
        (tw, th),
        (520.min(to_edge), 480),
        "a clicked turn takes an answer's own measure in the room it has, and hugs its words up \
         to a window's height"
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
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
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
        .find(|id| id.starts_with("tl:so.tilo.notes:root-"))
        .expect("the notes' root is published");

    // The agent's step in the turn, as the broker hands it to the session: its `place` names no
    // point, and stands beside the turn, as the MCP server fills it in for a turn's step.
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q2"),
            "shell:layout",
            "place",
            &serde_json::json!({"resource": notes}),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let (nx, ny, nw, nh) = wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.notes")
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
    // The rectangle's room keeps its size: what fills it does not hug.
    let filled = places(&session)
        .into_iter()
        .find(|place| place.resource.contains("so.tilo.notes"))
        .expect("the notes' place");
    let strip =
        crate::common::strip(&session, crate::common::number(&filled.strip)).expect("its strip");
    assert!(
        !strip.canvas_hugs,
        "what fills the rectangle keeps the rectangle's size: {strip:?}"
    );
}

/// A turn's place is the turn: an agent's step aimed at it — a `place` into the turn's strip, a
/// `show` of something else in the turn's place — stands what it shows in the room of the
/// rectangle under the turn, and beside the turn after that, and the turn's view stays.
#[test]
fn an_agents_step_aimed_at_the_turn_never_replaces_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.1, y + h * 0.1);
    let to = (x + w * 0.5, y + h * 0.95);
    frame_and_say(&mut session, from, to, "what do my notes say?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q4");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.starts_with("chat.workspace.turns:"))
    });
    let (tx, ty, tw, th) = canvas_box(&session, "chat.workspace.turns:").expect("the turn's box");
    let root_of = |session: &Session, app: &str| {
        let prefix = format!("tl:{app}:root-");
        session
            .resources()
            .into_iter()
            .map(|spec| spec.id.as_str().to_owned())
            .find(|id| id.starts_with(&prefix))
            .unwrap_or_else(|| panic!("{app}'s root is published"))
    };
    let notes = root_of(&session, "so.tilo.notes");
    let mails = root_of(&session, "so.tilo.mail");

    // A `place` into the turn's own strip, as a tab of it.
    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "place",
            &serde_json::json!({
                "resource": notes,
                "strip": crate::common::number(&turn.strip),
            }),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let (nx, ny, nw, _) = wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.notes")
    });
    assert!(
        nx == tx && nw == tw && ny >= ty + th,
        "the notes stand in the room under the turn, not in its place: {:?} under {:?}",
        (nx, ny, nw),
        (tx, ty, tw, th)
    );

    // A `show` of something else in the turn's own place.
    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "show",
            &serde_json::json!({
                "place": format!("shell:place:{}", turn.key),
                "resource": mails,
            }),
        )
        .unwrap_or_else(|error| panic!("the agent's show runs: {error}"));
    let (mx, _, _, _) = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    assert!(
        mx >= tx + tw,
        "with the room taken, the inbox stands beside the turn: {mx} against {:?}",
        (tx, tw)
    );
    let still = places(&session)
        .into_iter()
        .find(|place| place.key == turn.key)
        .expect("the turn's place still stands");
    assert!(
        still.resource.starts_with("chat.workspace.turns:") && still.shown_as == "turn",
        "the turn's place still shows the turn: {still:?}"
    );
    // The `show` named no view: the mail stands as the thing, its inbox's list, never Mail's
    // whole window with its sidebar of accounts (canvas.md §7, "The thing, never the app").
    let inbox = places(&session)
        .into_iter()
        .find(|place| place.resource.contains("so.tilo.mail"))
        .expect("the inbox's place");
    assert_eq!(
        inbox.title, "All inboxes",
        "the inbox's own view: {inbox:?}"
    );
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        !session.shell_says(MAIL_SIDEBAR),
        "no sidebar of the mail window around the inbox"
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
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
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
            "shell:layout",
            "place",
            serde_json::json!({"resource": "chat.workspace.channels:c_ask"})
                .as_object()
                .unwrap(),
        )
        .unwrap_or_else(|error| panic!("the conversation is placed: {error:?}"));
    let whole = wait_for(&mut session, "the conversation placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource == "chat.workspace.channels:c_ask")
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
            .find(|place| place.resource.contains("so.tilo.mail"))
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

/// The inbox, in the rectangle from `from` to `to`, and where it stands on screen.
fn inbox_in(
    session: &mut Session,
    from: (f64, f64),
    to: (f64, f64),
) -> (
    tilo_session_core::layout::PlaceReading,
    (f64, f64, f64, f64),
) {
    frame_and_say(session, from, to, "my mails");
    let inbox = wait_for(session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
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
        .find(|spec| spec.id.as_str() == inbox.resource)
        .map(|spec| spec.actions.iter().map(|a| a.label.clone()).collect())
        .unwrap_or_default();
    assert!(!offered.is_empty(), "the inbox offers actions: {inbox:?}");
    // The pointer over the place shows its handle; a right-click on its name opens the menu.
    point_to(&mut session, (px + pw * 0.5, py + ph * 0.5));
    // The handle names the place by its own title (its caption), a window by its name.
    let title = if inbox.title.is_empty() {
        &inbox.name
    } else {
        &inbox.title
    };
    let handle = wait_for(&mut session, "the place's handle", |session| {
        session
            .shell_controls(title)
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
        session.layout().borrow_mut().last_step(),
        session.shell_refusals()
    );
    let (_, _, nw, nh) = place_on_screen(&session, &inbox).expect("the inbox");
    assert!(
        near(nw, pw + 120.0) && near(nh, ph + 80.0),
        "the place takes the size its corner was dragged to: {:?} from {:?}",
        (nw, nh),
        (pw, ph)
    );
    // Its step is the person's in the workspace's layout history, which a session holds where it
    // holds the workspace (`common::last_step`); this one holds none, so the size is what is read.
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
    let edge = (f64::midpoint(from.0, to.0), to.1);
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

/// What the empty canvas says to assistive technology: the description its map carries, when it
/// carries one.
fn canvas_says(session: &Session) -> Option<String> {
    session.a11y_windows().into_iter().find_map(|(_, _, tree)| {
        tree.nodes.into_iter().find_map(|(_, node)| {
            (node.label() == Some(CANVAS))
                .then(|| node.description().map(str::to_owned))
                .flatten()
        })
    })
}

/// The canvas is the whole screen (`canvas.md` §7): no dock, and a bar that keeps only the time,
/// the notices and the person. The style and the workspaces are in the person's own menu,
/// never in a control that is always there, and choosing another style from it shows it.
#[test]
fn the_canvas_is_the_whole_screen() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the canvas's bar", |session| {
        on_screen(session, "Notifications").is_some()
            && crate::common::person_menu(session).is_some()
    });
    let labels = session.accessible_labels();
    for gone in ["Launcher", "Dock", "Workspaces", "Showing Frames", "Tilo"] {
        assert!(
            on_screen(&session, gone).is_none(),
            "the canvas keeps no {gone}: {labels:?}"
        );
    }
    // The person's menu holds the styles and the workspaces.
    let menu = crate::common::person_menu(&session).expect("the person's menu");
    crate::common::press(&mut session, &menu);
    until(&mut session, "the person's menu", |session| {
        on_screen(session, "Strips").is_some() && on_screen(session, "Workspaces").is_some()
    });
    crate::common::press(&mut session, "Strips");
    until(&mut session, "the strips", |session| {
        on_screen(session, CANVAS).is_none()
            && crate::common::preset_menu(session).as_deref() == Some("Showing Strips")
    });
    assert!(
        on_screen(&session, "Launcher").is_some(),
        "the strips keep their dock: the canvas alone is the whole screen"
    );
}

/// The empty canvas says once, quietly, what it is for; the first ask puts it away, and it stays
/// away when the canvas is empty again: it is the person's record, not the canvas's state.
#[test]
fn the_empty_canvas_says_once_what_it_is_for() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    until(&mut session, "the hint", |session| {
        canvas_says(session).as_deref() == Some("Click anywhere and say what you want")
    });
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.2, y + h * 0.2), "my mails");
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
    });
    assert!(
        canvas_says(&session).is_none(),
        "asked once, it says nothing"
    );
    // The canvas empty again: the place closed, as its handle's close does.
    session
        .layout()
        .borrow_mut()
        .ask(
            &crate::common::as_person(PERSON),
            tilo_session_core::layout::LayoutAction::Close {
                place: crate::common::number(&inbox.key),
            },
            tilo_session_core::layout::Busy::default(),
        )
        .unwrap_or_else(|error| panic!("the inbox closes: {error}"));
    until(&mut session, "the canvas empty", |session| {
        places(session).is_empty()
    });
    for _ in 0..10 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        canvas_says(&session).is_none(),
        "said once, it is not said again"
    );
}

/// A turn asked near the canvas's trailing edge stands wholly in view: the canvas pans to it and
/// never zooms. (That its card hugs its words is the map's, `map_canvas_tests`; this test's stand-in
/// for Chat publishes no reading of the turn for it to hug.)
#[test]
fn a_turn_asked_at_the_edge_is_panned_into_view() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let at = (x + w * 0.92, y + h * 0.1);
    point_and_say(&mut session, at, "what is on my task list?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q1");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.starts_with("chat.workspace.turns:"))
    });
    for _ in 0..60 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let (tx, ty, tw, th) = wait_for(&mut session, "the turn on screen", |session| {
        place_on_screen(session, &turn)
    });
    assert!(
        tx >= x - 0.5 && tx + tw <= x + w + 0.5 && ty >= y - 0.5 && ty + th <= y + h + 0.5,
        "the turn stands wholly in view: {:?} in {:?}",
        (tx, ty, tw, th),
        (x, y, w, h)
    );
    let (_, _, box_w, _) = canvas_box(&session, "chat.workspace.turns:").expect("its box");
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    let zoom = tw / box_w as f64;
    assert!(
        (zoom - 1.0).abs() < 0.02,
        "the canvas panned and kept its zoom: {zoom}"
    );
}

/// The person's turn wins over the agent's later placement (`canvas.md` §7, "Everything stays in
/// view"): what the agent stands beside the turn takes free room where the person is looking,
/// and a place the agent puts far off-screen is not chased at the cost of the turn, which stays
/// wholly in view, at the zoom the person had.
#[test]
fn the_turn_stays_in_view_over_the_agents_places() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let inside = |(px, py, pw, ph): (f64, f64, f64, f64)| {
        px >= x - 0.5 && px + pw <= x + w + 0.5 && py >= y - 0.5 && py + ph <= y + h + 0.5
    };
    point_and_say(
        &mut session,
        (x + w * 0.45, y + h * 0.2),
        "what is on my task list?",
    );
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q1");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.starts_with("chat.workspace.turns:"))
    });
    for _ in 0..60 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let shown = wait_for(&mut session, "the turn on screen", |session| {
        place_on_screen(session, &turn)
    });
    assert!(inside(shown), "the turn stands in view: {shown:?}");
    let root_of = |session: &Session, app: &str| {
        let prefix = format!("tl:{app}:root-");
        session
            .resources()
            .into_iter()
            .map(|spec| spec.id.as_str().to_owned())
            .find(|id| id.starts_with(&prefix))
            .unwrap_or_else(|| panic!("{app}'s root is published"))
    };
    // The agent's step in the turn shows what it shows beside the turn: in view.
    let notes = root_of(&session, "so.tilo.notes");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q1"),
            "shell:layout",
            "place",
            &serde_json::json!({"resource": notes}),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let placed = wait_for(&mut session, "the notes placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.notes"))
    });
    for _ in 0..60 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let beside = wait_for(&mut session, "the notes on screen", |session| {
        place_on_screen(session, &placed)
    });
    assert!(
        inside(beside),
        "what the agent stood beside the turn is in view: {beside:?}"
    );
    // Put with no size of its own, it hugs what it shows up to the room beside the turn, as a
    // thing found at a click does (`canvas.md` §7, "Places hug what they show").
    let strip =
        crate::common::strip(&session, crate::common::number(&placed.strip)).expect("its strip");
    assert!(
        strip.canvas_hugs,
        "the agent's place beside its turn hugs what it shows: {strip:?}"
    );
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    let most = strip.canvas_height as f64;
    assert!(
        beside.3 <= most + 0.5,
        "it stands no taller than its room: {} of {most}",
        beside.3
    );
    let shown = place_on_screen(&session, &turn).expect("the turn");
    assert!(inside(shown), "and the turn still is: {shown:?}");
    // A place the agent puts far off-screen: the canvas does not chase it.
    let mails = root_of(&session, "so.tilo.mail");
    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "place",
            &serde_json::json!({"resource": mails, "x": 6000, "y": 5000}),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    for _ in 0..60 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let shown = place_on_screen(&session, &turn).expect("the turn");
    assert!(
        inside(shown),
        "the turn stays wholly in view, not pushed out by a place off-screen: {shown:?}"
    );
    let (_, _, box_w, _) = canvas_box(&session, "chat.workspace.turns:").expect("its box");
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    let zoom = shown.2 / box_w as f64;
    assert!((zoom - 1.0).abs() < 0.02, "and kept its zoom: {zoom}");
}

/// The whole conversation is reached from the turn's handle, not a button in its card: what the
/// turn's own action runs stands Chat's page of the conversation beside the turn.
#[test]
fn the_whole_conversation_is_reached_from_the_turns_handle() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.1, y + h * 0.1), "what is due?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q4");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.starts_with("chat.workspace.turns:"))
    });
    let (tx, _, tw, _) = canvas_box(&session, "chat.workspace.turns:").expect("the turn's box");
    // What the turn's own `conversation` action runs, as its handle's menu offers it (the action
    // itself is the conversations service's, which this test's stand-in for Chat bypasses).
    session
        .layout()
        .borrow_mut()
        .open_conversation_of(&crate::common::as_person(PERSON), &turn.resource)
        .unwrap_or_else(|error| panic!("the turn opens its conversation: {error}"));
    let whole = wait_for(&mut session, "the conversation placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource == "chat.workspace.channels:c_ask")
    });
    assert_eq!(whole.shown_as, "page", "it stands as Chat's page");
    let (cx, _, _, _) = canvas_box(&session, "chat.workspace.channels:").expect("its box");
    assert!(
        cx >= tx + tw,
        "it stands beside the turn: {cx} beside {tx}+{tw}"
    );
}

/// What Mail's whole window says and its content never does: the title over its sidebar of every
/// account's places.
const MAIL_SIDEBAR: &str = "All accounts";

/// A note in the list of notes the canvas places for "my notes": the last the notebook holds, so a
/// row that is not the first note's page.
const SECOND_NOTE: &str = "Lentil soup";

/// Say `text` at a point of free ground on the canvas of a seeded session served from `root`.
fn say_on_the_canvas(root: &std::path::Path, text: &str) -> Session {
    let mut session = seeded_session(root);
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(&mut session, (x + w * 0.1, y + h * 0.2), text);
    session
}

/// "my mails" places the thing it names, the inbox's list of conversations, never Mail's window
/// (canvas.md §7, "The thing, never the app"): a conversation's row stands on the canvas, and the
/// window's sidebar of accounts does not.
#[test]
fn my_mails_places_the_message_list_not_the_mail_window() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = say_on_the_canvas(root.path(), "my mails");
    until(
        &mut session,
        "a conversation's row on the canvas",
        |session| session.shell_says(crate::common::SUBJECT),
    );
    assert!(
        !session.shell_says(MAIL_SIDEBAR),
        "the inbox's list stands with no sidebar of the mail window around it"
    );
}

/// "my notes" places the list of notes: every note a row, and no note's page, editor or the
/// notebook's file menu.
#[test]
fn my_notes_places_the_list_of_notes_not_the_notebook_window() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = say_on_the_canvas(root.path(), "my notes");
    until(&mut session, "the notes as rows on the canvas", |session| {
        session.shell_says(crate::common::NOTE_TITLE) && session.shell_says(SECOND_NOTE)
    });
    assert!(
        session.shell_controls("File").is_empty(),
        "no notebook window's file menu stands on the canvas"
    );
    assert!(
        session.shell_controls("Write something").is_empty()
            && !session.shell_says("Write something"),
        "no note's editor stands on the canvas"
    );
}

/// Words naming the app alone place the app: "open Mail" stands Mail's whole window, its sidebar
/// of accounts with it.
#[test]
fn open_mail_places_the_whole_app() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = say_on_the_canvas(root.path(), "open Mail");
    until(&mut session, "Mail's window with its sidebar", |session| {
        session.shell_says(MAIL_SIDEBAR) && session.shell_says(crate::common::SUBJECT)
    });
}

/// An agent showing the person's mail stands the same thing typing "my mails" does: the
/// layout's `place` of Mail's root with no view places the inbox's list, never Mail's window
/// (canvas.md §4, §7). The step is the layout's own published `place`, as the broker runs an
/// agent's.
#[test]
fn placing_mails_root_without_a_view_stands_the_message_list() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mail = session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with("tl:so.tilo.mail:root-"))
        .map(|spec| spec.id.as_str().to_owned())
        .expect("Mail's root is published");
    session
        .invoke(
            "shell:layout",
            "place",
            serde_json::json!({ "resource": mail }).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the mail is placed: {error:?}"));
    until(
        &mut session,
        "a conversation's row on the canvas",
        |session| session.shell_says(crate::common::SUBJECT),
    );
    assert!(
        !session.shell_says(MAIL_SIDEBAR),
        "the inbox's list stands with no sidebar of the mail window around it"
    );
}

/// What the agent opens in its turn stands as the thing, never its app (canvas.md §7, "The thing,
/// never the app"): the layout's `open` of Tasks' root, naming no view, as the broker runs the
/// agent's step, stands Tasks' place view, the list of what is to do, as content: no sidebar of
/// the app's lists, no window frame.
#[test]
fn the_agents_open_of_an_apps_root_stands_its_place_view() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(
        &mut session,
        (x + w * 0.1, y + h * 0.2),
        "remind me to call mum tomorrow at 6pm",
    );
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q1");
    wait_for(&mut session, "the turn placed", |session| {
        canvas_box(session, "chat.workspace.turns:")
    });
    let tasks = root_of(&session, "so.tilo.lang.tasks");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q1"),
            "shell:layout",
            "open",
            &serde_json::json!({"resource": tasks}),
        )
        .unwrap_or_else(|error| panic!("the agent's open runs: {error}"));
    let placed = wait_for(&mut session, "the tasks placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.lang.tasks"))
    });
    // The views Tasks publishes for its root: the one marked for a place (`place view`), and its
    // default view, the whole app.
    let views = session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str() == tasks)
        .map(|spec| spec.views)
        .expect("Tasks' root is published");
    let place_view = views
        .iter()
        .find(|view| view.place)
        .map(|view| view.id.clone())
        .expect("Tasks publishes a place view of its root");
    let app_view = views
        .iter()
        .find(|view| view.primary)
        .map(|view| view.id.clone())
        .expect("Tasks' root has a default view");
    assert_ne!(place_view, app_view, "the place view is not the app's own");
    assert_eq!(
        placed.shown_as, place_view,
        "Tasks' root stands through its place view: {placed:?}"
    );
    assert_ne!(
        placed.shown_as, app_view,
        "never through the app's default view: {placed:?}"
    );
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        !session.shell_says("Projects"),
        "no sidebar of the app's lists stands on the canvas"
    );
}

/// Chat holding each ask sent so far, oldest first, as the turn of `conversation` that
/// `questions` names for it: [`chat_holds`] for several asks at once.
fn chat_holds_each(session: &Session, conversation: &str, questions: &[&str]) {
    let next = std::cell::Cell::new(0_usize);
    let stood = session.layout().borrow_mut().stand_asked(
        &|_| {
            let question = questions.get(next.get())?;
            next.set(next.get() + 1);
            let turn = format!("chat.workspace.turns:{question}");
            let asked = tilo_session_core::layout::Asked {
                conversation: conversation.to_owned(),
                message: (*question).to_owned(),
                turn: turn.clone(),
                opened_as: format!(
                    "chat.workspace.channels:{}",
                    conversation.trim_start_matches("chat.workspace/channels/")
                ),
            };
            let mut published =
                tilo_actions::ResourceSpec::new(turn, "chat.workspace.turns", *question).with_view(
                    tilo_actions::ResourceViewSpec::new("turn", *question, "open"),
                );
            published.app_id = Some("chat".to_owned());
            Some((asked, vec![published]))
        },
        tilo_session_core::layout::Busy::default(),
    );
    assert_eq!(stood, questions.len(), "each ask waited for its turn");
}

/// The canvas box the turn `question` stands in.
fn turn_box(session: &Session, question: &str) -> Option<(i64, i64, i64, i64)> {
    canvas_box(session, &format!("chat.workspace.turns:{question}"))
}

/// A tilolang app's root, as the session publishes it.
fn root_of(session: &Session, app: &str) -> String {
    let prefix = format!("tl:{app}:root-");
    session
        .resources()
        .into_iter()
        .map(|spec| spec.id.as_str().to_owned())
        .find(|id| id.starts_with(&prefix))
        .unwrap_or_else(|| panic!("{app}'s root is published"))
}

/// Whether the box `what` fills the room under the turn at `turn`.
fn in_room(what: (i64, i64, i64, i64), turn: (i64, i64, i64, i64)) -> bool {
    what.0 == turn.0 && what.2 == turn.2 && what.1 >= turn.1 + turn.3
}

/// Two asks in quick succession in one conversation, each in a rectangle of its own: the second
/// is asked before the first's agent placed anything. Each turn's step, as the broker hands it to
/// the session in that turn, fills that turn's own rectangle — never the conversation's latest.
#[test]
fn two_asks_in_quick_succession_each_fill_their_own_rectangle() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let first = ((x + w * 0.04, y + h * 0.08), (x + w * 0.46, y + h * 0.95));
    let second = ((x + w * 0.54, y + h * 0.08), (x + w * 0.96, y + h * 0.95));

    frame_and_say(&mut session, first.0, first.1, "what do my notes say?");
    frame_and_say(&mut session, second.0, second.1, "what is in my inbox?");
    until(&mut session, "both asks sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| sent.len() >= 2))
            .unwrap_or(false)
    });
    chat_holds_each(&session, CONVERSATION, &["q5", "q6"]);
    let one = wait_for(&mut session, "the first turn placed", |session| {
        turn_box(session, "q5")
    });
    let two = wait_for(&mut session, "the second turn placed", |session| {
        turn_box(session, "q6")
    });
    let notes = root_of(&session, "so.tilo.notes");
    let mails = root_of(&session, "so.tilo.mail");

    // The first turn's agent answers only now, after the second was asked; then the second's.
    for (message, resource) in [("q5", &notes), ("q6", &mails)] {
        session
            .invoke_in_turn(
                &crate::common::agent("assistant"),
                (CONVERSATION, message),
                "shell:layout",
                "place",
                &serde_json::json!({ "resource": resource }),
            )
            .unwrap_or_else(|error| panic!("the agent's place in {message} runs: {error}"));
    }
    let notes_at = wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.notes")
    });
    let mails_at = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    assert!(
        in_room(notes_at, one),
        "the first turn's notes fill its own rectangle under it: {notes_at:?} under {one:?}"
    );
    assert!(
        in_room(mails_at, two),
        "the second turn's inbox fills its own rectangle under it: {mails_at:?} under {two:?}"
    );
}

/// A turn's room is kept from when the person asks: the agent's first step can come before the
/// turn's view stands (Chat answered the send with its message, its record does not show it yet),
/// and it fills the rectangle the person dragged all the same. The turn then stands over it, and
/// the room is not given out twice.
#[test]
fn a_step_before_its_turn_stands_fills_the_turns_rectangle() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let from = (x + w * 0.1, y + h * 0.1);
    let to = (x + w * 0.5, y + h * 0.95);
    frame_and_say(&mut session, from, to, "what do my notes say?");
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    // Chat answered the ask's send with the message it posted; Chat's process is not part of this
    // session, so this stands in for that answer, as `chat_holds` stands in for its record.
    session
        .layout()
        .borrow()
        .know_posted(std::rc::Rc::new(|_| Some("q7".to_owned())));
    let notes = root_of(&session, "so.tilo.notes");
    let mails = root_of(&session, "so.tilo.mail");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q7"),
            "shell:layout",
            "place",
            &serde_json::json!({ "resource": notes }),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let notes_at = wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.notes")
    });
    assert!(
        turn_box(&session, "q7").is_none(),
        "the turn's view does not stand yet"
    );

    chat_holds_each(&session, CONVERSATION, &["q7"]);
    let turn = wait_for(&mut session, "the turn placed", |session| {
        turn_box(session, "q7")
    });
    assert!(
        in_room(notes_at, turn),
        "the notes fill the room under the turn: {notes_at:?} under {turn:?}"
    );
    // The room is taken: the turn's next step stands beside the turn.
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q7"),
            "shell:layout",
            "place",
            &serde_json::json!({ "resource": mails }),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let mails_at = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    assert!(
        mails_at.0 >= turn.0 + turn.2,
        "with the room taken, the inbox stands beside the turn: {mails_at:?} beside {turn:?}"
    );
}

/// A turn asked by a click is a place that hugs its words: the layout takes the height its card
/// is drawn at, so the canvas under a short answer stays free for the next ask, rather than the
/// next turn being put below the turn's tallest and the camera panning to it. Its caption is its
/// question.
#[test]
fn a_turns_place_hugs_its_words() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, _) = on_screen(&session, CANVAS).expect("the canvas");
    point_and_say(
        &mut session,
        (x + w * 0.2, y + 40.0),
        "remind me about the dentist",
    );
    until(&mut session, "the ask sent", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array().map(|sent| !sent.is_empty()))
            .unwrap_or(false)
    });
    chat_holds(&session, CONVERSATION, "q30");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("chat.workspace.turns:q30"))
    });
    let strip =
        crate::common::strip(&session, crate::common::number(&turn.strip)).expect("its strip");
    assert!(
        strip.canvas_hugs,
        "a turn's place hugs its words: {strip:?}"
    );
}

/// A click left of a turn, closer to it than a turn is wide: the new turn stands at the click,
/// narrowed to the free area before the turn on its right, never moved past the screen's edge with
/// the camera panning after it.
#[test]
fn a_turn_asked_left_of_another_stands_at_its_click_narrowed() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let sent = |session: &mut Session, count: usize| {
        until(session, "the ask sent", |session| {
            session.settle_services();
            session
                .read("session:send-to")
                .and_then(|read| read["sent"].as_array().map(|sent| sent.len() >= count))
                .unwrap_or(false)
        });
    };
    point_and_say(&mut session, (x + w * 0.45, y + h * 0.3), "hi");
    sent(&mut session, 1);
    chat_holds(&session, CONVERSATION, "q32");
    let (first_x, _, _, _) = wait_for(&mut session, "the first turn placed", |session| {
        turn_box(session, "q32")
    });
    let at = (x + w * 0.05, y + h * 0.35);
    point_and_say(&mut session, at, "what time is it");
    sent(&mut session, 2);
    chat_holds(&session, CONVERSATION, "q33");
    let (sx, _, sw, _) = wait_for(&mut session, "the second turn placed", |session| {
        turn_box(session, "q33")
    });
    #[allow(clippy::cast_possible_truncation, reason = "canvas pixels")]
    let asked_x = (at.0 - x) as i64;
    assert!(
        (sx - asked_x).abs() <= 24,
        "the second turn stands at its click ({asked_x}): {sx}"
    );
    assert!(
        sx + sw <= first_x,
        "narrowed to the area before the first turn ({first_x}): {sx}+{sw}"
    );
}

/// A click just left of a turn, with less free area before it than a turn's least width: the new
/// turn stands in the free spot nearest the click that is on the screen, and the camera stays,
/// so the first turn stands where it stood.
#[test]
fn a_turn_asked_with_nothing_free_at_its_click_stands_in_view_and_nothing_pans() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let sent = |session: &mut Session, count: usize| {
        until(session, "the ask sent", |session| {
            session.settle_services();
            session
                .read("session:send-to")
                .and_then(|read| read["sent"].as_array().map(|sent| sent.len() >= count))
                .unwrap_or(false)
        });
    };
    let steps = |session: &mut Session| {
        for _ in 0..30 {
            session.step(Duration::from_millis(16)).unwrap();
        }
    };
    let turn = |session: &Session, question: &str| {
        let needle = format!("chat.workspace.turns:{question}");
        places(session)
            .into_iter()
            .find(|place| place.resource.contains(&needle))
    };
    point_and_say(&mut session, (x + w * 0.3, y + h * 0.2), "hi");
    sent(&mut session, 1);
    chat_holds(&session, CONVERSATION, "q32");
    let first = wait_for(&mut session, "the first turn placed", |session| {
        turn(session, "q32")
    });
    steps(&mut session);
    let before = wait_for(&mut session, "the first turn on screen", |session| {
        place_on_screen(session, &first)
    });
    // Closer to the turn than a turn's least width: nothing free at the click itself.
    point_and_say(
        &mut session,
        (before.0 - 40.0, before.1 + 20.0),
        "what time is it",
    );
    sent(&mut session, 2);
    chat_holds(&session, CONVERSATION, "q33");
    let second = wait_for(&mut session, "the second turn placed", |session| {
        turn(session, "q33")
    });
    steps(&mut session);
    let (sx, sy, sw, sh) = wait_for(&mut session, "the second turn on screen", |session| {
        place_on_screen(session, &second)
    });
    assert!(
        sx >= x - 1.0 && sy >= y - 1.0 && sx + sw <= x + w + 1.0 && sy + sh <= y + h + 1.0,
        "the second turn stands whole on the screen ({x},{y} {w}x{h}): {sx},{sy} {sw}x{sh}"
    );
    let after = place_on_screen(&session, &first).expect("the first turn still on screen");
    assert!(
        (after.0 - before.0).abs() < 1.0 && (after.1 - before.1).abs() < 1.0,
        "the camera stayed: the first turn stands where it stood ({before:?}): {after:?}"
    );
}

/// The live order: one turn asked in a dragged rectangle, whose agent places a thing, then a second
/// asked by a click elsewhere. The second turn's agent `place` stands what it shows beside the
/// second turn: never above the first, and never at the canvas' origin as a place that names no
/// turn does.
#[test]
fn a_clicked_turns_place_stands_beside_it_after_a_rectangles_turn() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let sent = |session: &mut Session, count: usize| {
        until(session, "the ask sent", |session| {
            session.settle_services();
            session
                .read("session:send-to")
                .and_then(|read| read["sent"].as_array().map(|sent| sent.len() >= count))
                .unwrap_or(false)
        });
    };

    frame_and_say(
        &mut session,
        (x + w * 0.02, y + h * 0.05),
        (x + w * 0.3, y + h * 0.6),
        "summarise what changed in my notes this week",
    );
    sent(&mut session, 1);
    chat_holds(&session, CONVERSATION, "q9");
    let first = wait_for(&mut session, "the first turn placed", |session| {
        turn_box(session, "q9")
    });
    let notes = root_of(&session, "so.tilo.notes");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q9"),
            "shell:layout",
            "place",
            &serde_json::json!({ "resource": notes }),
        )
        .unwrap_or_else(|error| panic!("the first turn's place runs: {error}"));
    wait_for(&mut session, "the notes placed", |session| {
        canvas_box(session, "so.tilo.notes")
    });

    // A click on free canvas, below everything the first turn stood.
    let below = places(&session)
        .iter()
        .filter_map(|place| canvas_box(&session, &place.resource))
        .map(|(_, py, _, ph)| py + ph)
        .max()
        .unwrap_or_default();
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    let at = (x + w * 0.5, (y + below as f64 + y + h) / 2.0);
    point_and_say(&mut session, at, "show me my mails");
    sent(&mut session, 2);
    chat_holds(&session, CONVERSATION, "q10");
    let (bx, by, bw, bh) = wait_for(&mut session, "the second turn placed", |session| {
        turn_box(session, "q10")
    });
    let mails = root_of(&session, "so.tilo.mail");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q10"),
            "shell:layout",
            "place",
            &serde_json::json!({ "resource": mails }),
        )
        .unwrap_or_else(|error| panic!("the second turn's place runs: {error}"));
    let (mx, my, mw, mh) = wait_for(&mut session, "the inbox placed", |session| {
        canvas_box(session, "so.tilo.mail")
    });
    // Adjacent to the turn on any side, a spot in view first (canvas.md §3): beside it is level
    // with some of it, raised or lowered as far as staying in view asks.
    let level = my < by + bh && my + mh > by;
    let trailing = mx >= bx + bw && level;
    let leading = mx + mw <= bx && level;
    let below = my >= by + bh && mx >= bx && mx < bx + bw;
    assert!(
        trailing || leading || below,
        "the inbox stands beside the second turn: {:?} beside {:?} (the first turn at {first:?})",
        (mx, my),
        (bx, by, bw, bh)
    );
}

/// The canvas in the order a person used it when every frame stopped laying out: a question asked
/// near the top, its turn; a question asked near the foot, its turn and the inbox the agent
/// opened beside it; then "my notes" found at a click left of the inbox; then a pan and a zoom.
/// Every frame lays out: the shell reports no frame skipped and no part that did not settle, and
/// every place stays drawn. (The turn's long scrolling answer itself is the toolkit's, in
/// `scrollable::gutter_tests`: Chat's process is not part of this session, so its turns here stand
/// in for its record.)
#[test]
fn the_canvas_lays_out_every_frame_through_asks_an_inbox_and_a_find() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let sent = |session: &mut Session, count: usize| {
        until(session, "the ask sent", |session| {
            session.settle_services();
            session
                .read("session:send-to")
                .and_then(|read| read["sent"].as_array().map(|sent| sent.len() >= count))
                .unwrap_or(false)
        });
    };
    let steps = |session: &mut Session| {
        for _ in 0..30 {
            session.step(Duration::from_millis(16)).unwrap();
        }
    };

    point_and_say(
        &mut session,
        (x + w * 0.3, y + h * 0.1),
        "which of my tasks are due this week?",
    );
    sent(&mut session, 1);
    chat_holds(&session, CONVERSATION, "q20");
    wait_for(&mut session, "the first turn placed", |session| {
        turn_box(session, "q20")
    });
    steps(&mut session);

    point_and_say(
        &mut session,
        (x + w * 0.3, y + h * 0.85),
        "show me my mails",
    );
    sent(&mut session, 2);
    chat_holds(&session, CONVERSATION, "q21");
    wait_for(&mut session, "the second turn placed", |session| {
        turn_box(session, "q21")
    });
    let mails = root_of(&session, "so.tilo.mail");
    session
        .invoke_in_turn(
            &crate::common::agent("assistant"),
            (CONVERSATION, "q21"),
            "shell:layout",
            "place",
            &serde_json::json!({ "resource": mails }),
        )
        .unwrap_or_else(|error| panic!("the agent's place runs: {error}"));
    let inbox = wait_for(&mut session, "the inbox placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.contains("so.tilo.mail"))
    });
    steps(&mut session);
    wait_for(&mut session, "the inbox on screen", |session| {
        place_on_screen(session, &inbox)
    });
    // Free canvas on screen: a point clear of every place there and the caption over it, wherever
    // the camera has framed them.
    let boxes: Vec<(f64, f64, f64, f64)> = places(&session)
        .iter()
        .filter_map(|place| place_on_screen(&session, place))
        .collect();
    let clear = |(px, py): (f64, f64)| {
        boxes.iter().all(|&(bx, by, bw, bh)| {
            px < bx - 24.0 || px > bx + bw + 24.0 || py < by - 48.0 || py > by + bh + 24.0
        })
    };
    // Free canvas is where a click puts the caret, as the person finds it: a point the boxes
    // above miss but a card covers takes no caret, and the next one is tried.
    (0..12)
        .flat_map(|row| (0..16).map(move |column| (column, row)))
        .map(|(column, row)| {
            (
                x + w * 0.03 + f64::from(column) * 80.0,
                y + h * 0.15 + f64::from(row) * 56.0,
            )
        })
        .filter(|&(px, py)| px < x + w - 24.0 && py < y + h * 0.9 && clear((px, py)))
        .find(|&point| {
            click(&mut session, point);
            // A caret stands within frames of the click, or the click took none.
            let caret = (0..20).any(|_| {
                session
                    .step(std::time::Duration::from_millis(16))
                    .expect("the session steps");
                on_screen(&session, CANVAS_CARET).is_some()
            });
            if !caret {
                key(&mut session, KEY_ESCAPE);
            }
            caret
        })
        .unwrap_or_else(|| panic!("free canvas on screen among {boxes:?}"));

    crate::common::say(&mut session, "my notes");
    until(&mut session, "the notes as rows on the canvas", |session| {
        session.shell_says(crate::common::NOTE_TITLE)
    });
    steps(&mut session);
    pan_and_zoom(&mut session, (-120.0, -80.0));
    steps(&mut session);

    let reports = session.take_shell_layout_reports();
    assert!(
        reports.is_empty(),
        "every frame laid out and settled: {reports:#?}"
    );
    for place in places(&session) {
        assert!(
            place_on_screen(&session, &place).is_some()
                || canvas_box(&session, &place.resource).is_some(),
            "{} stays drawn",
            place.resource
        );
    }
}

/// The canvas's camera, driven the way `tilo-control` drives it over the test-control socket:
/// a middle drag pans it, the arrow keys then pan it from the keyboard that press gave it, and a
/// wheel with Ctrl held zooms it about the pointer. A place on it moves and grows with the lens.
#[test]
fn the_test_control_socket_pans_and_zooms_the_canvas() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let (inbox, before) = inbox_in(
        &mut session,
        (x + w * 0.3, y + h * 0.3),
        (x + w * 0.55, y + h * 0.6),
    );
    let at = |session: &mut Session, why: &str, moved: &dyn Fn((f64, f64, f64, f64)) -> bool| {
        wait_for(session, why, |session| {
            place_on_screen(session, &inbox).filter(|now| moved(*now))
        })
    };
    let ask = |session: &mut Session, request: serde_json::Value| {
        crate::common::control(session, &socket, &request)
    };
    let free = (x + w * 0.85, y + h * 0.2);

    // A middle drag across free ground pans the canvas by the drag.
    let dragged = ask(
        &mut session,
        serde_json::json!({
            "op": "drag", "button": "middle",
            "x_from": free.0, "y_from": free.1,
            "x_to": free.0 - 100.0, "y_to": free.1 - 60.0, "steps": 6,
        }),
    );
    assert_eq!(dragged, serde_json::json!({"ok": {}}), "the drag is taken");
    let panned = at(&mut session, "the inbox panned with the drag", &|now| {
        near(now.0, before.0 - 100.0) && near(now.1, before.1 - 60.0)
    });

    // The middle press gave the canvas the keyboard: an arrow key pans it a line.
    let pressed = ask(
        &mut session,
        serde_json::json!({"op": "key", "key": "ArrowRight"}),
    );
    assert_eq!(pressed, serde_json::json!({"ok": {}}), "the key is taken");
    let keyed = at(&mut session, "the inbox panned by the arrow key", &|now| {
        (now.0 - panned.0).abs() > 1.0 && near(now.1, panned.1)
    });

    // A wheel turned up with Ctrl held, over free ground, zooms in about the pointer: the place
    // grows on screen.
    let zoomed = ask(
        &mut session,
        serde_json::json!({
            "op": "scroll", "unit": "notch", "x": free.0, "y": free.1, "dy": 2.0, "ctrl": true,
        }),
    );
    assert_eq!(zoomed, serde_json::json!({"ok": {}}), "the wheel is taken");
    at(&mut session, "the inbox grown by the zoom", &|now| {
        now.2 > keyed.2 + 4.0 && now.3 > keyed.3 + 4.0
    });

    // The wheel's Ctrl was let go with it: a wheel without it pans rather than zooms.
    let grown = place_on_screen(&session, &inbox).expect("the inbox on screen");
    let scrolled = ask(
        &mut session,
        serde_json::json!({
            "op": "scroll", "unit": "notch", "x": free.0, "y": free.1, "dy": -1.0,
        }),
    );
    assert_eq!(scrolled, serde_json::json!({"ok": {}}));
    at(
        &mut session,
        "the inbox panned by the plain wheel",
        &|now| near(now.2, grown.2) && (now.1 - grown.1).abs() > 1.0,
    );
}

/// A notice's banner floats over the canvas: what the person had on screen keeps the place it
/// stands in when the banner rises, and the banner's own × takes the notice away without moving
/// it either. The banner is a card drawn over the canvas, never a band between the bar and the
/// canvas, so a notice arriving never shifts the canvas or what it shows.
#[test]
fn a_notice_floats_over_the_canvas_and_never_moves_what_stands() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let (x, y, w, h) = on_screen(&session, CANVAS).expect("the canvas");
    let (inbox, (px, py, pw, ph)) = inbox_in(
        &mut session,
        (x + w * 0.1, y + h * 0.3),
        (x + w * 0.5, y + h * 0.85),
    );
    let before = on_screen(&session, CANVAS).expect("the canvas");
    // A notice arrives as the session posts one, as chat's does when someone writes.
    session.notices().post(
        tilo_session_core::desktop::notifications::Notification::new("Chat", "Guest wrote to you")
            .with_app_name("Chat"),
    );
    let dismiss = wait_for(&mut session, "the banner's dismiss", |session| {
        session.shell_control("Dismiss")
    });
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    // The banner's own card stands inside the canvas's bounds, under the bar: it is laid over
    // the canvas, not above it.
    assert!(
        dismiss.1 >= before.1 && dismiss.1 + dismiss.3 <= before.1 + before.3,
        "the banner's dismiss stands over the canvas {before:?}: {dismiss:?}"
    );
    // Neither the canvas nor the place moved: what the person had on screen stays put.
    let after = on_screen(&session, CANVAS).expect("the canvas");
    assert!(
        near(after.0, before.0)
            && near(after.1, before.1)
            && near(after.2, before.2)
            && near(after.3, before.3),
        "the canvas keeps its bounds: {before:?} then {after:?}"
    );
    let now = place_on_screen(&session, &inbox).expect("the inbox");
    assert!(
        near(now.0, px) && near(now.1, py) && near(now.2, pw) && near(now.3, ph),
        "the inbox keeps its place: {:?} then {now:?}",
        (px, py, pw, ph)
    );
    // The banner's × takes the notice away, as the person pressing it does, and the canvas still
    // stands where it did.
    crate::common::press(&mut session, "Dismiss");
    until(&mut session, "the banner gone", |session| {
        session.shell_control("Dismiss").is_none()
    });
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    let gone = on_screen(&session, CANVAS).expect("the canvas");
    assert!(
        near(gone.0, before.0)
            && near(gone.1, before.1)
            && near(gone.2, before.2)
            && near(gone.3, before.3),
        "the canvas keeps its bounds once the banner goes: {before:?} then {gone:?}"
    );
    let still = place_on_screen(&session, &inbox).expect("the inbox");
    assert!(
        near(still.0, px) && near(still.1, py) && near(still.2, pw) && near(still.3, ph),
        "the inbox keeps its place once the banner goes: {:?} then {still:?}",
        (px, py, pw, ph)
    );
    assert!(
        session.shell_refusals().is_empty(),
        "the banner's press reached the layout: {:?}",
        session.shell_refusals()
    );
}
