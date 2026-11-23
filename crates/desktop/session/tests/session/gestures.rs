//! Touchpad gestures the desktop takes: a four-finger swipe moves between workspaces and a pinch in
//! opens the overview, both as the person's setting says.

use tilo_compositor::GestureEvent;

use super::*;

fn gesture(session: &mut Session, events: &[GestureEvent]) {
    for event in events {
        session
            .compositor()
            .input(InputEvent::Gesture(*event))
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

#[test]
fn touchpad_gestures_the_desktop_takes_switch_workspaces_and_open_the_overview() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let first = active_workspace(&session);
    invoke(&mut session, CHROME_RESOURCE, "new-workspace");
    let second = active_workspace(&session);
    assert_ne!(first, second, "a new workspace opened");
    // Four fingers down go to the workspace before, under natural scrolling (the default): the
    // workspaces follow the fingers.
    gesture(
        &mut session,
        &[
            GestureEvent::SwipeBegin { fingers: 4 },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: 60.0 },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: 60.0 },
            GestureEvent::SwipeEnd { cancelled: false },
        ],
    );
    assert_eq!(active_workspace(&session), first, "back to the first");

    let before = session.accessible_labels();
    gesture(
        &mut session,
        &[
            GestureEvent::PinchBegin { fingers: 3 },
            GestureEvent::PinchUpdate {
                dx: 0.0,
                dy: 0.0,
                scale: 0.6,
                rotation: 0.0,
            },
            GestureEvent::PinchEnd { cancelled: false },
        ],
    );
    session.step(Duration::from_millis(1)).unwrap();
    assert_ne!(
        session.accessible_labels(),
        before,
        "a pinch in opened the overview"
    );
}
