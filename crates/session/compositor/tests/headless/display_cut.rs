//! A window the desktop cuts to its display draws and takes the pointer there only, and a press
//! with Super held over a window is the desktop's, which carries the window with it.

use super::*;

/// Every window at one place, cut to the left 100 pixels of it.
struct Cut;

impl WindowPolicy for Cut {
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        StackPolicy.initial_size(window, outputs)
    }

    fn place(
        &mut self,
        _: &WindowInfo,
        _: (i32, i32),
        _: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        Point::from((100, 100))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        Some(
            windows
                .iter()
                .map(|(window, _)| Arrangement {
                    id: window.id,
                    location: Point::from((100, 100)),
                    size: None,
                    clip: Some(Rectangle::new((0, 0).into(), (200, 480).into())),
                    fullscreen: None,
                })
                .collect(),
        )
    }
}

#[test]
fn a_window_cut_to_its_display_shows_and_takes_the_pointer_there_only() {
    let mut compositor = headless(Box::new(Cut));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Rc::new(RefCell::new(
        Square::new((0, 0, 640, 480), [0, 0, 255, 255], Rc::clone(&log)).below(),
    )));
    let mut client = mapped_client(&mut compositor, "cut", (200, 100), [255, 0, 0, 255]);
    compositor.draw_frame().expect("a frame");
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(150, 150),
        Some([255, 0, 0, 255]),
        "the window inside its cut"
    );
    assert_eq!(
        frame.pixel(250, 150),
        Some([0, 0, 255, 255]),
        "past its cut the desktop shows, not the window"
    );

    compositor
        .input(InputEvent::PointerMotion { x: 250.0, y: 150.0 })
        .unwrap();
    assert_eq!(
        log.borrow().motions.last(),
        Some(&(250.0, 150.0)),
        "past its cut the pointer is the desktop's, not the window's"
    );
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.frames_done > 0
    });
}

/// The desktop under the windows, which carries a window pressed with Super held.
struct Carrier(Square);

impl SurfaceHost for Carrier {
    fn layers(&self) -> Vec<HostLayer> {
        self.0.layers()
    }

    fn below_windows(&self) -> bool {
        true
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.0.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.0.pointer_motion(x, y);
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.0.pointer_button(button, pressed);
    }

    fn carries_window_at(&mut self, _x: f64, _y: f64) -> bool {
        true
    }
}

#[test]
fn a_press_with_super_over_a_window_is_the_desktops_until_it_ends() {
    let mut compositor = headless(Box::new(StackPolicy));
    let log = Rc::new(RefCell::new(HostLog::default()));
    compositor.add_host(Rc::new(RefCell::new(Carrier(Square::new(
        (0, 0, 640, 480),
        [0, 0, 255, 255],
        Rc::clone(&log),
    )))));
    let mut client = mapped_client(&mut compositor, "carried", (200, 100), [255, 0, 0, 255]);
    compositor.draw_frame().expect("a frame");
    compositor
        .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.pointer_inside
    });

    compositor
        .input(InputEvent::Key {
            keycode: KEY_LEFTMETA,
            pressed: true,
        })
        .unwrap();
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        })
        .unwrap();
    // Carried over the window and past it, the pointer stays the desktop's.
    for x in [330.0, 400.0, 600.0] {
        compositor
            .input(InputEvent::PointerMotion { x, y: 240.0 })
            .unwrap();
    }
    compositor
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        })
        .unwrap();
    compositor
        .input(InputEvent::Key {
            keycode: KEY_LEFTMETA,
            pressed: false,
        })
        .unwrap();
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        !clients[0].seen.pointer_inside
    });
    assert_eq!(
        log.borrow().buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "the desktop has the press and its end"
    );
    assert!(
        log.borrow().motions.contains(&(330.0, 240.0)),
        "and the pointer over the window while the press lasts: {:?}",
        log.borrow().motions
    );
    assert!(
        client.seen.buttons.is_empty(),
        "the window never saw the press"
    );
}
