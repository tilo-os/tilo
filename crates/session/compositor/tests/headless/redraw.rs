//! What makes an output owe a frame: a change to what it shows, wherever the
//! change is made — the client's commit, or the policy placing its window.

use super::*;

/// A floating stack whose windows a test places, as a desk's strip pans them.
#[derive(Default)]
struct Movable {
    at: Rc<RefCell<Option<Point<i32, Logical>>>>,
}

impl WindowPolicy for Movable {
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        StackPolicy.initial_size(window, outputs)
    }

    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let at = *self.at.borrow();
        at.unwrap_or_else(|| StackPolicy.place(window, size, outputs))
    }

    fn arrange(
        &mut self,
        windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        let at = (*self.at.borrow())?;
        Some(
            windows
                .iter()
                .map(|(window, _)| Arrangement {
                    id: window.id,
                    location: at,
                    size: None,
                })
                .collect(),
        )
    }
}

/// Draw the frames the desk owes until it rests.
///
/// A frame that asks the policy for its places again may be the one that owes the next: the frame
/// that finds the move marks what it shows changed, and the display builds that frame for its own
/// blank. So the move takes the frame that discovers it and, when the display is not yet due, the
/// one the display paces it to — and a wait on the debt covers both, while a frame drawn before the
/// blank would show nothing.
fn settle(compositor: &mut Compositor) {
    loop {
        compositor.draw_frame().expect("a frame");
        if !compositor.owes_frame() {
            return;
        }
    }
}

/// A window the policy moves back onto an output is drawn there: the output
/// owes the frame that shows it, with nothing else having damaged the screen.
#[test]
fn a_window_the_policy_moves_is_drawn_where_it_moved_to() {
    let policy = Movable::default();
    let at = Rc::clone(&policy.at);
    let mut compositor = headless(Box::new(policy));
    let _client = mapped_client(&mut compositor, "moved", (200, 100), [255, 0, 0, 255]);
    settle(&mut compositor);

    // Off every output, as a window whose well stands off the strip is left.
    *at.borrow_mut() = Some(Point::from((-4000, 0)));
    settle(&mut compositor);
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "a window off the output is on no frame"
    );

    // Back on it: what the output shows changed, so it owes that frame and draws it, though no
    // client drew anything.
    let presented = compositor.frames_presented();
    *at.borrow_mut() = Some(Point::from((20, 20)));
    settle(&mut compositor);
    assert!(
        compositor.frames_presented() > presented,
        "the output shows the moved window in a frame of its own"
    );
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(100, 60),
        Some([255, 0, 0, 255]),
        "the window is drawn where the policy moved it to"
    );
}

/// A key the focused client takes changes nothing the compositor draws: the client answers it with
/// a commit, and only that owes a frame. A key that owed a frame of its own drew an empty frame
/// first, which then held the client's answer back to the display's next refresh.
#[test]
fn a_key_for_a_client_owes_no_frame_of_its_own() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "typed", (200, 100), [0, 0, 255, 255]);
    settle(&mut compositor);
    assert!(!compositor.owes_frame(), "the desk rests");
    for pressed in [true, false] {
        compositor
            .input(InputEvent::Key {
                keycode: 30,
                pressed,
            })
            .unwrap();
    }
    assert!(!compositor.owes_frame(), "a key alone owes no frame");
    compositor
        .input(InputEvent::PointerMotion { x: 50.0, y: 50.0 })
        .unwrap();
    assert!(compositor.owes_frame(), "the pointer moving does");
}
