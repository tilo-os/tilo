//! The touchpad's shell gestures under the tilolang shell: the fingers ask the desktop the session
//! owns what its chords and published actions ask, and the shell shows what changed.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::json;
use tilo_compositor::{GestureEvent, InputEvent};
use tilo_session::Session;

use crate::common::{on_screen, until};
use crate::layout_shortcuts::{CHROME, new_strip, open, session, settle, strips, workspace};

/// A swipe of `fingers` fingers travelling `(dx, dy)`, landed, moved and lifted as a touchpad
/// reports one.
fn swipe(session: &mut Session, fingers: u32, dx: f64, dy: f64) {
    let mut events = vec![GestureEvent::SwipeBegin { fingers }];
    events.extend(travel(dx, dy));
    events.push(GestureEvent::SwipeEnd { cancelled: false });
    fingers_on_the_pad(session, &events);
}

/// Fingers on the pad travelling `(dx, dy)` in the steps a touchpad reports.
fn travel(dx: f64, dy: f64) -> Vec<GestureEvent> {
    let steps = 12;
    (0..steps)
        .map(|_| GestureEvent::SwipeUpdate {
            dx: dx / f64::from(steps),
            dy: dy / f64::from(steps),
        })
        .collect()
}

/// A pinch of three fingers to `scale`.
fn pinch(session: &mut Session, scale: f64) {
    let steps = 10;
    let mut events = vec![GestureEvent::PinchBegin { fingers: 3 }];
    events.extend((1..=steps).map(|step| GestureEvent::PinchUpdate {
        dx: 0.0,
        dy: 0.0,
        scale: 1.0 + (scale - 1.0) * f64::from(step) / f64::from(steps),
        rotation: 0.0,
    }));
    events.push(GestureEvent::PinchEnd { cancelled: false });
    fingers_on_the_pad(session, &events);
}

/// Hand `events` to the seat as its touchpad reports them, a frame apart.
fn fingers_on_the_pad(session: &mut Session, events: &[GestureEvent]) {
    for event in events {
        session
            .compositor()
            .input(InputEvent::Gesture(*event))
            .expect("the touchpad's gesture");
        session
            .step(std::time::Duration::from_millis(8))
            .expect("a frame");
    }
    settle(session);
}

/// Three fingers up or down: the strip before or after the one shown, and back.
#[test]
fn three_fingers_up_and_down_step_the_strips() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let first = strips(&session).0[0];
    let made = new_strip(&mut session);
    until(&mut session, "on the new strip", |session| {
        strips(session).1 == Some(made)
    });

    swipe(&mut session, 3, 0.0, 300.0);
    let stepped = strips(&session).1;
    if stepped == Some(made) {
        swipe(&mut session, 3, 0.0, -300.0);
    }
    until(
        &mut session,
        "the fingers stepped to the first strip",
        |session| strips(session).1 == Some(first),
    );
    let back = if stepped == Some(made) { 300.0 } else { -300.0 };
    swipe(&mut session, 3, 0.0, back);
    until(&mut session, "and back to the new one", |session| {
        strips(session).1 == Some(made)
    });
}

/// Four fingers up or down: the workspace above or below the one in front, and back. The
/// compositor switches workspaces on vertical four-finger swipes and leaves sideways ones alone.
#[test]
fn four_fingers_switch_the_workspace() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let home = workspace(&session);
    session
        .invoke(
            CHROME,
            "new-workspace",
            json!({ "name": "Taxes", "background": true })
                .as_object()
                .unwrap(),
        )
        .expect("a second workspace");
    settle(&mut session);
    assert_eq!(workspace(&session), home);

    for dy in [-400.0, 400.0] {
        swipe(&mut session, 4, 0.0, dy);
        if workspace(&session) != home {
            break;
        }
    }
    until(
        &mut session,
        "the fingers went to the other workspace",
        |session| workspace(session) != home,
    );
    let there = workspace(&session);
    for dy in [400.0, -400.0] {
        swipe(&mut session, 4, 0.0, dy);
        if workspace(&session) != there {
            break;
        }
    }
    until(&mut session, "and back", |session| {
        workspace(session) == home
    });
}

/// A pinch in opens the shell's overview; a pinch out closes it.
#[test]
fn a_pinch_opens_and_closes_the_overview() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 1);
    let overview = tilo_session_core::desktop::overlays::Overlay::Overview.name();
    pinch(&mut session, 0.5);
    until(&mut session, "the overview", |session| {
        crate::common::shell_here(session) == overview
    });
    pinch(&mut session, 2.0);
    until(&mut session, "the overview closed", |session| {
        crate::common::shell_here(session) != overview
    });
}

/// Three fingers sideways carry the shown strip's row with them while they are down, and lifted
/// while moving, the row glides on the way they went.
#[test]
fn three_fingers_carry_the_strip_and_it_glides_on() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 6);
    // The row keeps the window in front in view, so a window picked by its place among six may
    // stand off the screen with its bar out of reach: the marker is one of the windows the shell
    // draws, and the direction it moves when the fingers carry the row is what is asserted.
    let beside = crate::common::wait_for(&mut session, "a window on the screen", |session| {
        crate::common::places(session)
            .into_iter()
            .map(|place| {
                if place.title.is_empty() {
                    place.name
                } else {
                    place.title
                }
            })
            .find(|name| on_screen(session, name).is_some())
    });
    let at = |session: &Session| on_screen(session, &beside).map(|(x, ..)| x);
    let before = at(&session).unwrap_or_else(|| panic!("{beside} stands on the screen"));

    let mut down = vec![GestureEvent::SwipeBegin { fingers: 3 }];
    down.extend(travel(150.0, 0.0));
    fingers_on_the_pad(&mut session, &down);
    let held = at(&session).expect("the window is still drawn");
    assert!(
        (held - before - 150.0).abs() <= 2.0,
        "the row went with the fingers: {before} then {held}"
    );

    let mut lifted = travel(150.0, 0.0);
    lifted.push(GestureEvent::SwipeEnd { cancelled: false });
    fingers_on_the_pad(&mut session, &lifted);
    for _ in 0..30 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("a frame");
    }
    let rested = at(&session).expect("the window is still drawn");
    assert!(
        rested > held + 150.0,
        "the row glides past where the fingers lifted: {held} then {rested}"
    );
}

/// The strip in front presents its pan beside the tree, and the published `pan` verb moves it:
/// the fingers pan the shell's own view, and a host or an agent that pans by the action moves
/// what the desktop presents.
#[test]
fn the_strip_in_front_presents_its_pan_and_the_pan_verb_moves_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut session, notes) = session();
    open(&mut session, &notes, 6);
    settle(&mut session);
    let strip = strips(&session).1.expect("a strip in front");
    let presented = session
        .handle()
        .presentation()
        .pan
        .expect("the strip in front presents its pan");
    assert_eq!(
        presented.strip,
        strip.0.to_string(),
        "the pan presented is the strip in front's"
    );
    assert!(
        session
            .handle()
            .snapshot("check", vec![])
            .desktop_pan
            .is_some(),
        "the navigation snapshot presents the pan too"
    );

    session
        .invoke(
            CHROME,
            "pan",
            json!({ "strip": strip.0.to_string(), "offset": 120.0 })
                .as_object()
                .expect("an object"),
        )
        .expect("the strip pans");
    settle(&mut session);
    let panned = session
        .handle()
        .presentation()
        .pan
        .expect("the strip still presents its pan");
    assert_eq!(
        panned.offset,
        120.0_f64.min(presented.max),
        "the pan went where the action asked, as far as the strip scrolls"
    );
}
