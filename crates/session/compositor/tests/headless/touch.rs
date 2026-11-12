//! Touch on the windows under it: the seat's touch objects where the client
//! bound them, and a pointer press where it did not.

use super::*;

#[test]
fn touches_reach_the_window_under_them_and_hosted_surfaces_as_presses() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Rc::new(RefCell::new(Square::new(
        (10, 10, 40, 40),
        [0, 255, 0, 255],
        Rc::clone(&log),
    ))));
    // Centred: the window spans (220, 190) to (420, 290).
    let mut client = mapped_client(&mut compositor, "touched", (200, 100), [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.touch.is_some()
    });

    for event in [
        InputEvent::TouchDown {
            slot: 0,
            x: 300.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        // Dragged off the window, the finger stays the window's.
        InputEvent::TouchMotion {
            slot: 0,
            x: 500.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        InputEvent::TouchUp { slot: 0 },
        InputEvent::TouchFrame,
    ] {
        compositor.input(event).unwrap();
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.touches.len() >= 6
    });
    assert_eq!(
        client.seen.touches,
        [
            "down 0 80 50",
            "frame",
            "motion 0 280 50",
            "frame",
            "up 0",
            "frame"
        ]
    );
    assert!(client.seen.keyboard_focused, "a touch focuses like a click");

    compositor
        .input(InputEvent::TouchDown {
            slot: 1,
            x: 20.0,
            y: 20.0,
        })
        .unwrap();
    compositor
        .input(InputEvent::TouchMotion {
            slot: 1,
            x: 25.0,
            y: 20.0,
        })
        .unwrap();
    compositor.input(InputEvent::TouchUp { slot: 1 }).unwrap();
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    assert_eq!(log.borrow().motions, [(20.0, 20.0), (25.0, 20.0)]);
    assert_eq!(log.borrow().buttons, [(BTN_LEFT, true), (BTN_LEFT, false)]);
    assert_eq!(log.borrow().left, 1);
    assert_eq!(client.seen.touches.len(), 6, "the window saw none of it");
}

#[test]
fn a_window_that_binds_no_wl_touch_is_touched_as_a_pointer() {
    let mut compositor = headless(Box::new(StackPolicy));
    // Centred: the window spans (220, 190) to (420, 290).
    let mut client =
        mapped_client_without_touch(&mut compositor, "pointerly", (200, 100), [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pointer.is_some()
    });
    assert!(
        client.seen.touch.is_none(),
        "the client left the touch objects alone"
    );

    for event in [
        InputEvent::TouchDown {
            slot: 0,
            x: 300.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        // Dragged off the window, the finger stays the window's.
        InputEvent::TouchMotion {
            slot: 0,
            x: 500.0,
            y: 240.0,
        },
        InputEvent::TouchFrame,
        InputEvent::TouchUp { slot: 0 },
        InputEvent::TouchFrame,
    ] {
        compositor.input(event).unwrap();
    }
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.buttons.len() >= 2
    });
    assert!(
        client.seen.touches.is_empty(),
        "no touch object, no touch event"
    );
    assert_eq!(
        client.seen.buttons,
        [(BTN_LEFT, true), (BTN_LEFT, false)],
        "the finger presses where it went down and lifts where it rose"
    );
    assert_eq!(
        client.seen.pointer_entered_at,
        Some((80.0, 50.0)),
        "the pointer presses where the finger went down"
    );
    assert_eq!(
        client.seen.pointer_positions,
        [(280.0, 50.0), (280.0, 50.0)],
        "the pointer follows the finger, even off the window"
    );
}
