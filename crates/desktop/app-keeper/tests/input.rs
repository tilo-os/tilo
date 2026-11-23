//! Pointer locks, relative motion and touchpad gestures through a keeper.

use tilo_compositor::{GestureEvent, InputEvent};

use crate::support::{App, Rig};

const RED: [u8; 4] = [255, 0, 0, 255];

fn centre(rig: &Rig) -> (f64, f64) {
    let (_, _, (x, y, w, h)) = rig.windows()[0].clone();
    (
        f64::from(x) + f64::from(w) / 2.0,
        f64::from(y) + f64::from(h) / 2.0,
    )
}

fn point_at(rig: &mut Rig, app: &mut App) {
    let (x, y) = centre(rig);
    rig.desktop()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    rig.pump(&mut [app], |_, apps| apps[0].heard.pointer_entered);
}

#[test]
fn a_pointer_lock_and_relative_motion_cross_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Game", Some("game"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);
    point_at(&mut rig, &mut app);

    app.make_relative_pointer();
    app.lock_pointer();
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.locked);
    rig.desktop()
        .input(InputEvent::PointerMotionRelative { dx: 5.0, dy: -3.0 })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| {
        !apps[0].heard.relative_motion.is_empty()
    });

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        app.heard.unlocked,
        "the app is told its pointer is free while the desktop is away"
    );

    app.heard.locked = false;
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    point_at(&mut rig, &mut app);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.locked);
    rig.settle(&mut [&mut app]);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_two_finger_swipe_reaches_the_app_and_after_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Zoom", Some("zoom"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);
    point_at(&mut rig, &mut app);
    app.make_gestures();
    rig.settle(&mut [&mut app]);

    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::SwipeBegin { fingers: 2 }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::SwipeUpdate {
            dx: 12.0,
            dy: 0.0,
        }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::SwipeEnd {
            cancelled: false,
        }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::PinchBegin { fingers: 2 }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::PinchUpdate {
            dx: 1.0,
            dy: 0.0,
            scale: 1.5,
            rotation: 0.0,
        }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::PinchEnd {
            cancelled: false,
        }))
        .unwrap();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |_, apps| {
        turns += 1;
        (!apps[0].heard.swipes.is_empty()
            && !apps[0].heard.swipe_updates.is_empty()
            && !apps[0].heard.swipe_ends.is_empty()
            && !apps[0].heard.pinches.is_empty()
            && !apps[0].heard.pinch_updates.is_empty()
            && !apps[0].heard.pinch_ends.is_empty())
            || turns > 200
    });
    assert!(
        !app.heard.pinches.is_empty()
            && !app.heard.pinch_updates.is_empty()
            && !app.heard.pinch_ends.is_empty(),
        "the pinch reached the app: swipes={:?} updates={:?} ends={:?} pinches={:?} pinch_updates={:?} pinch_ends={:?}",
        app.heard.swipes,
        app.heard.swipe_updates,
        app.heard.swipe_ends,
        app.heard.pinches,
        app.heard.pinch_updates,
        app.heard.pinch_ends
    );
    assert_eq!(app.heard.swipes[0].1, 2);
    assert_eq!(app.heard.pinches[0].1, 2);

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    point_at(&mut rig, &mut app);

    app.heard.swipes.clear();
    app.heard.swipe_updates.clear();
    app.heard.swipe_ends.clear();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::SwipeBegin { fingers: 2 }))
        .unwrap();
    rig.desktop()
        .input(InputEvent::Gesture(GestureEvent::SwipeEnd {
            cancelled: false,
        }))
        .unwrap();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |_, apps| {
        turns += 1;
        !apps[0].heard.swipes.is_empty() || turns > 200
    });
    assert!(
        !app.heard.swipes.is_empty(),
        "gestures keep reaching the app after a restart"
    );
}
