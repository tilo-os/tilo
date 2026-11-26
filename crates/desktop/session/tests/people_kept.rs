//! Who is here, kept on screen by the tilolang shell: the roster's own button keeps it at an edge
//! of the shell rather than in the bar, the edge is the person's setting and comes back after a
//! restart, a drag of its header moves it to the edge nearest where it is let go (and a press that
//! never leaves the slop, or a drag called off with Escape, moves nothing), and it waits away while
//! nobody else is here, coming back when someone arrives.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use settings_store::widgets::{DockEdge, PEOPLE_OVERLAY};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};

use crate::common::{HEIGHT, WIDTH, press, wait_for};

const BTN_LEFT: u32 = 0x110;
const KEY_ESC: u32 = 1;
/// The kept roster's own button, which puts it back in the bar.
const PUT_BACK: &str = "Put back in the bar";
/// The bar's roster's button that keeps it on screen.
const KEEP: &str = "Keep on screen";

fn start() -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: "Pat".to_owned(),
            sync: Some(crate::common::untethered("Pat")),
            // The person's settings, which keep the roster's edge: the service a computer's
            // session runs them in.
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// Run frames until the bar stands: the person's own face is on it, and who is here is read.
fn ready(session: &mut Session) {
    until(session, "the bar", |session| {
        session.shell_control("Pat: open your card").is_some()
    });
}

/// Mia here, as the peer network lists her; or nobody but the person.
fn mia_here(session: &Session, here: bool) {
    let members = if here {
        vec![tilo_session_core::desktop::here::LiveMember {
            publisher: format!("{:064x}", 1),
            kind: "person".to_owned(),
            label: "Mia Clayton".to_owned(),
            ..tilo_session_core::desktop::here::LiveMember::default()
        }]
    } else {
        Vec::new()
    };
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(members));
}

/// Where the shell's control named `name` stands, once it stood still for two frames.
#[track_caller]
fn standing(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    wait_for(session, format!("{name} to stand still"), |session| {
        let mut place = session.shell_control(name)?;
        for _ in 0..2 {
            session.step(Duration::from_millis(16)).expect("step");
            let next = session.shell_control(name)?;
            if next != place {
                return None;
            }
            place = next;
        }
        Some(place)
    })
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, what, done);
}

fn pointer(session: &mut Session, (x, y): (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    session.step(Duration::from_millis(16)).unwrap();
}

fn left(session: &mut Session, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed,
        })
        .unwrap();
    session.step(Duration::from_millis(16)).unwrap();
}

/// Take the kept roster by its header, away from its button, move the pointer to `to` in steps
/// as a hand does, and leave the button held.
fn take_header_to(session: &mut Session, to: (f64, f64)) {
    let (x, y, _, h) = standing(session, PUT_BACK);
    let from = (x - 120.0, y + h / 2.0);
    pointer(session, from);
    left(session, true);
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        pointer(
            session,
            (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
        );
    }
}

/// The edge the person's settings keep the roster at, and whether it is kept on screen.
fn kept() -> (bool, DockEdge) {
    let kept = PEOPLE_OVERLAY.get();
    (kept.pinned, kept.edge)
}

/// No surface of the shell's own stands up: the bar's roster is closed.
fn resting(session: &Session) -> bool {
    matches!(crate::common::shell_here(session).as_str(), "" | "none")
}

fn centre((x, y, w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + w / 2.0, y + h / 2.0)
}

/// The roster's own button keeps it on screen at the right edge, a drag of its header moves it to
/// the bottom-left corner, a press inside the slop and a drag called off with Escape leave it there,
/// and after a restart it waits at that corner again, without being kept a second time. Its button
/// puts it back in the bar, and the edge stays the person's for the next time.
#[test]
fn who_is_here_kept_on_screen_moves_between_edges_and_keeps_its_edge_after_a_restart() {
    let (_home, _settings) = crate::common::settings_home();
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = start();
    ready(&mut session);
    mia_here(&session, true);
    // Who is here opens, as the bar's faces open it: Mia's row in it.
    session
        .invoke("session:people", "show", &serde_json::Map::new())
        .expect("who is here opens");
    until(&mut session, "Mia in the roster", |session| {
        session.shell_control("Mia Clayton").is_some() && session.shell_control(KEEP).is_some()
    });
    assert!(
        session.shell_control(PUT_BACK).is_none(),
        "nothing is kept on screen until the person keeps it"
    );
    let widgets = crate::common::widgets_offered(&session);
    assert!(
        widgets
            .iter()
            .any(|(_, actions)| actions.iter().any(|action| action == "keep-people")),
        "the person's widgets section offers keeping who is here: {widgets:?}"
    );

    // The bar's roster keeps itself on screen: it closes, and the roster waits at the right edge.
    press(&mut session, KEEP);
    let right = standing(&mut session, PUT_BACK);
    assert!(
        resting(&session),
        "the bar's own roster closed: {}",
        crate::common::shell_here(&session)
    );
    assert_eq!(
        kept(),
        (true, DockEdge::Right),
        "kept in the person's settings"
    );
    assert!(
        right.0 > f64::from(WIDTH) * 0.75 && right.1 > 0.0 && right.1 + right.3 < f64::from(HEIGHT),
        "it waits at the right edge, on the screen: {right:?}"
    );
    let (_, middle) = centre(right);
    assert!(
        middle < f64::from(HEIGHT) * 0.75,
        "beside the work, not in a corner: {right:?}"
    );
    // Dragged by its header to the bottom-left corner, it moves there and the setting follows.
    take_header_to(&mut session, (60.0, f64::from(HEIGHT) - 40.0));
    // Held, it goes with the hand.
    let held = session
        .shell_control(PUT_BACK)
        .expect("the roster stands while it is held");
    assert!(
        held.0 < right.0 - 200.0,
        "the roster follows the hand: held at {held:?}, from {right:?}"
    );
    left(&mut session, false);
    until(&mut session, "the roster at the bottom-left corner", |_| {
        kept() == (true, DockEdge::BottomLeft)
    });
    let corner = standing(&mut session, PUT_BACK);
    assert!(
        corner.0 < f64::from(WIDTH) * 0.5 && centre(corner).1 > middle,
        "it stands at the bottom-left corner: {corner:?}"
    );

    // A press that never leaves the slop is not a drag: it stays where it is.
    let (x, y, _, h) = corner;
    pointer(&mut session, (x - 120.0, y + h / 2.0));
    left(&mut session, true);
    pointer(&mut session, (x - 118.0, y + h / 2.0));
    left(&mut session, false);
    assert_eq!(kept(), (true, DockEdge::BottomLeft));
    assert_eq!(standing(&mut session, PUT_BACK), corner);

    // A drag to the right edge called off with Escape springs back to its corner, and the
    // setting is untouched; the button let go afterwards moves nothing either.
    take_header_to(
        &mut session,
        (f64::from(WIDTH) - 60.0, f64::from(HEIGHT) / 2.0),
    );
    crate::common::key(&mut session, KEY_ESC);
    left(&mut session, false);
    assert_eq!(
        kept(),
        (true, DockEdge::BottomLeft),
        "the drag was called off"
    );
    assert_eq!(
        standing(&mut session, PUT_BACK),
        corner,
        "it sprang back to its corner"
    );

    // After a restart it waits at the same corner, from the person's settings alone.
    drop(session);
    let mut session = start();
    ready(&mut session);
    mia_here(&session, true);
    let again = standing(&mut session, PUT_BACK);
    assert_eq!(again, corner, "it came back where the person left it");

    // Its button puts it back in the bar; the corner stays theirs for the next time.
    press(&mut session, PUT_BACK);
    until(&mut session, "the roster back in the bar", |session| {
        session.shell_control(PUT_BACK).is_none()
    });
    assert_eq!(kept(), (false, DockEdge::BottomLeft));
}

/// Kept on screen while nobody else is here, the roster waits away, its pin kept; it comes in when
/// someone arrives and waits away again when they leave.
#[test]
fn who_is_here_kept_on_screen_waits_away_while_nobody_else_is_here() {
    let (_home, _settings) = crate::common::settings_home();
    let _settled = tilo_ui_base::motion::testing::settled();
    // Kept on screen at the bottom-right corner, as the person's settings keep it (Settings, a
    // script or an agent keep it the same way, by the section's `keep-people`).
    crate::common::kept_before(|settings| {
        PEOPLE_OVERLAY
            .set_in(
                settings,
                settings_store::widgets::PeopleOverlayPrefs {
                    pinned: true,
                    edge: DockEdge::BottomRight,
                },
            )
            .expect("the roster is kept on screen");
    });
    let mut session = start();
    ready(&mut session);
    // The session reads the edge from the person's settings as it starts: the roster is kept.
    let widgets =
        settings_sections::address("settings.widgets").expect("a section the session keeps");
    let pinned = |session: &Session| {
        session
            .read(&widgets)
            .and_then(|read| read.get("people_overlay")?.get("pinned")?.as_bool())
            == Some(true)
    };
    for _ in 0..60 {
        if pinned(&session) {
            break;
        }
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        pinned(&session),
        "the session reads the roster kept on screen: it reads {widgets} as {:?}, and the \
         settings hold {:?}",
        session.read(&widgets),
        PEOPLE_OVERLAY.get()
    );
    for _ in 0..8 {
        session.step(Duration::from_millis(16)).unwrap();
    }
    assert!(
        session.shell_control(PUT_BACK).is_none(),
        "alone, nothing of the roster stands on the shell: {:?}",
        session.accessible_labels()
    );

    // Mia arrives: the roster comes in at its corner, nothing pressed again.
    mia_here(&session, true);
    let corner = standing(&mut session, PUT_BACK);
    assert!(
        corner.0 > f64::from(WIDTH) * 0.5 && corner.1 > f64::from(HEIGHT) * 0.5,
        "at the bottom-right corner: {corner:?}"
    );
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Mia Clayton"),
        "it says who is here"
    );

    // She leaves: it waits away again, its pin kept.
    mia_here(&session, false);
    until(&mut session, "the roster waiting away", |session| {
        session.shell_control(PUT_BACK).is_none()
    });
    assert_eq!(kept(), (true, DockEdge::BottomRight));
}
